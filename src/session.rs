/// session.rs — Encapsula uma sessão PTY completa (aba).
///
/// # BufferCache + LayoutIndex
/// O `BufferCache` mantém os `GlyphonBuffer` invalidados por versão.
/// O `LayoutIndex` é construído junto com o cache e provê hit-testing
/// O(log k) por coordenada de tela → (block_idx, line, col).
use crate::{BlockStore, OutputLine, StyledSpan, TerminalState, PtyHandle, TerminalEvents, spawn_pty};
use glyphon::{
    Attrs, Buffer as GlyphonBuffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight,
};

// Implementação de TerminalEvents para a Session
struct SessionEvents {
    // Aqui poderíamos ter um canal para notificar a UI thread para redesenhar
}

impl TerminalEvents for SessionEvents {
    fn on_content_changed(&self) {
        // No futuro: enviar evento de repaint para o winit
    }
    fn on_title_changed(&self, _title: String) {
        // No futuro: atualizar título da aba
    }
}

pub const FONT_H: f32 = 18.0;
pub const TAB_BAR_H: f32 = 40.0;
pub const INPUT_BOX_H: f32 = 48.0;
pub const MARGIN_X: f32 = 16.0;
pub const BLOCK_PAD_X: f32 = 12.0;
pub const BLOCK_PAD_Y: f32 = 8.0;
pub const BLOCK_GAP: f32 = 8.0;
pub const COL_CMD: (u8, u8, u8) = (0x00, 0xFF, 0x9F); // Mint/Aqua vibrante
pub const COL_FG: (u8, u8, u8) = (0xF0, 0xF0, 0xF0);  // Branco quase puro para leitura

// ---------------------------------------------------------------------------
// Hit-testing
// ---------------------------------------------------------------------------

/// Resultado de um hit-test: identifica exatamente onde na hierarquia lógica
/// um ponto de tela (x, y) cai.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitResult {
    /// Índice do bloco no snapshot atual (0 = mais antigo visível).
    pub block_idx: usize,
    /// Linha dentro do bloco (0 = linha do comando, 1+ = output).
    pub line: usize,
    /// Coluna aproximada (baseada em FONT_W — não usa shaping reverso).
    pub col: usize,
}

/// Bounding box de um bloco renderizado — coordenadas de tela em pixels.
#[derive(Debug, Clone, Copy)]
pub struct BlockRect {
    pub y0: f32,
    pub y1: f32,
    /// Posição X do texto (após padding)
    pub x0: f32,
}

/// Índice espacial construído a cada `rebuild_buffers`.
/// Permite hit-testing O(log k) via busca binária em `rects` (ordenados por y0).
pub struct LayoutIndex {
    /// Um rect por bloco, na mesma ordem que `block_buffers`.
    pub rects: Vec<BlockRect>,
}

impl LayoutIndex {
    pub fn new() -> Self {
        Self { rects: Vec::new() }
    }

    /// Mapeia coordenadas de tela (x, y) → HitResult.
    ///
    /// Complexidade: O(log k) para encontrar o bloco (busca binária por y),
    /// depois O(1) para calcular linha e coluna via divisão inteira.
    ///
    /// Retorna `None` se (x, y) não cair sobre nenhum bloco.
    pub fn hit_test(&self, x: f32, y_abs: f32, font_w: f32) -> Option<HitResult> {
        if self.rects.is_empty() { return None; }
        
        let y = y_abs - TAB_BAR_H;
        if y < 0.0 { return None; }

        // Busca binária: encontra o bloco cujo y0 <= y < y1
        let idx = self.rects.partition_point(|r| r.y0 <= y);
        let idx = idx.saturating_sub(1);
        let rect = &self.rects[idx];

        if y < rect.y0 || y >= rect.y1 { return None; }

        // Linha dentro do bloco (y relativo ao topo do conteúdo, após padding)
        let content_y = (y - rect.y0 - BLOCK_PAD_Y).max(0.0);
        let line = (content_y / FONT_H) as usize;

        // Coluna: x relativo ao início do texto
        let col = ((x - rect.x0).max(0.0) / font_w) as usize;

        Some(HitResult { block_idx: idx, line, col })
    }
}

// ---------------------------------------------------------------------------
// BufferCache
// ---------------------------------------------------------------------------

pub struct BufferCache {
    pub version: u64,
    pub block_buffers: Vec<GlyphonBuffer>,
    pub input_buffer: GlyphonBuffer,
    pub block_tops: Vec<f32>,
    pub block_heights: Vec<f32>,
    /// Índice espacial — reconstruído junto com os buffers.
    pub layout: LayoutIndex,
}

impl BufferCache {
    pub fn new(font_system: &mut FontSystem) -> Self {
        let mut input_buffer = GlyphonBuffer::new(font_system, Metrics::new(FONT_H * 0.78, FONT_H));
        input_buffer.set_size(font_system, Some(100.0), Some(INPUT_BOX_H));
        Self {
            version: u64::MAX,
            block_buffers: Vec::new(),
            input_buffer,
            block_tops: Vec::new(),
            block_heights: Vec::new(),
            layout: LayoutIndex::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct Session {
    pub pty: PtyHandle,
    pub blocks: BlockStore,
    #[allow(dead_code)] // mantém a thread vte-processor viva via Arc interno
    pub terminal: TerminalState,
    pub input_line: String,
    pub cache: BufferCache,
    #[allow(dead_code)] // usado na tab bar (Fase 4)
    pub title: String,
}

impl Session {
    pub fn new(cols: u16, rows: u16, font_system: &mut FontSystem, title: impl Into<String>) -> anyhow::Result<Self> {
        let events = std::sync::Arc::new(SessionEvents {});
        let blocks = BlockStore::new(events.clone());
        let terminal = TerminalState::new(blocks.clone(), events);
        let mut pty = spawn_pty(cols, rows)?;

        {
            let terminal_clone = terminal.clone();
            let output_rx = pty.take_output_rx();
            std::thread::Builder::new()
                .name("vte-processor".into())
                .spawn(move || {
                    for chunk in output_rx {
                        terminal_clone.process_bytes(&chunk);
                    }
                })?;
        }

        Ok(Self {
            pty,
            blocks,
            terminal,
            input_line: String::new(),
            cache: BufferCache::new(font_system),
            title: title.into(),
        })
    }

    /// Reconstrói buffers de texto e o LayoutIndex quando a versão mudou.
    pub fn rebuild_buffers(&mut self, font_system: &mut FontSystem, w: f32, _scroll_h: f32) {
        let block_w = w - MARGIN_X * 2.0 - BLOCK_PAD_X * 2.0;
        let text_x = MARGIN_X + BLOCK_PAD_X;
        let (finished, active) = self.blocks.snapshot();

        self.cache.block_buffers.clear();
        self.cache.block_tops.clear();
        self.cache.block_heights.clear();
        self.cache.layout.rects.clear();

        let mut y = BLOCK_GAP;

        let all_blocks: Vec<_> = finished.iter().chain(std::iter::once(&active)).collect();

        for block in &all_blocks {
            let trimmed = block.trimmed_lines();
            let partial = block.current_partial();
            let has_partial = !partial.is_empty();

            if block.command.is_empty() && trimmed.is_empty() && !has_partial {
                continue;
            }

            let n_output = trimmed.len() + if has_partial { 1 } else { 0 };
            let n_lines = if block.command.is_empty() { 0 } else { 1 } + n_output;
            let content_h = (n_lines.max(1) as f32) * FONT_H + BLOCK_PAD_Y * 2.0;

            // Registra bounding box no LayoutIndex antes de avançar y
            self.cache.layout.rects.push(BlockRect {
                y0: y,
                y1: y + content_h,
                x0: text_x,
            });

            let mut spans: Vec<(String, Attrs)> = Vec::new();

            if !block.command.is_empty() {
                spans.push((
                    format!("$ {}\n", block.command),
                    Attrs::new()
                        .family(Family::Monospace)
                        .color(Color::rgb(COL_CMD.0, COL_CMD.1, COL_CMD.2))
                        .weight(Weight::BOLD),
                ));
            }

            for line in trimmed {
                push_line_spans(&mut spans, line);
                spans.push(("\n".to_string(), Attrs::new().family(Family::Monospace)));
            }

            for span in partial {
                push_span_attrs(&mut spans, span);
            }

            let span_refs: Vec<(&str, Attrs)> =
                spans.iter().map(|(s, a)| (s.as_str(), a.clone())).collect();

            let mut buf = GlyphonBuffer::new(font_system, Metrics::new(FONT_H * 0.78, FONT_H));
            buf.set_size(font_system, Some(block_w), Some(content_h));
            buf.set_rich_text(
                font_system,
                span_refs,
                &Attrs::new().family(Family::Monospace),
                Shaping::Basic,
                None,
            );
            buf.shape_until_scroll(font_system, false);

            self.cache.block_tops.push(y);
            self.cache.block_heights.push(content_h);
            self.cache.block_buffers.push(buf);

            y += content_h + BLOCK_GAP;
        }

        // Input buffer
        let input_display = format!("❯  {}_", self.input_line);
        let mut input_buf = GlyphonBuffer::new(font_system, Metrics::new(FONT_H * 0.78, FONT_H));
        input_buf.set_size(font_system, Some(w - MARGIN_X * 2.0), Some(INPUT_BOX_H));
        input_buf.set_text(
            font_system,
            &input_display,
            &Attrs::new().family(Family::Monospace).color(Color::rgb(0xFF, 0xFF, 0xFF)),
            Shaping::Basic,
            None,
        );
        input_buf.shape_until_scroll(font_system, false);
        self.cache.input_buffer = input_buf;
    }

    pub fn update_input_buffer(&mut self, font_system: &mut FontSystem, _w: f32) {
        let input_display = format!("❯  {}_", self.input_line);
        self.cache.input_buffer.set_text(
            font_system,
            &input_display,
            &Attrs::new().family(Family::Monospace).color(Color::rgb(0xFF, 0xFF, 0xFF)),
            Shaping::Basic,
            None,
        );
        self.cache.input_buffer.shape_until_scroll(font_system, false);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_line_spans(spans: &mut Vec<(String, Attrs)>, line: &OutputLine) {
    for span in &line.0 {
        push_span_attrs(spans, span);
    }
}

fn push_span_attrs(spans: &mut Vec<(String, Attrs)>, span: &StyledSpan) {
    let mut a = Attrs::new()
        .family(Family::Monospace)
        .color(Color::rgb(span.r, span.g, span.b));
    if span.bold   { a = a.weight(Weight::BOLD); }
    if span.italic { a = a.style(Style::Italic); }
    spans.push((span.text.clone(), a));
}

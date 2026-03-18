/// terminal_state.rs — Parser VTE que alimenta tanto a grade quanto o BlockStore.
use crate::TerminalEvents;
use std::sync::{Arc, Mutex};
use vte::{Params, Parser, Perform};
use crate::block_store::{BlockStore, StyledSpan};

// ---------------------------------------------------------------------------
// Tipos de cor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TermColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl TermColor {
    pub fn to_rgb(self, is_fg: bool) -> (u8, u8, u8) {
        match self {
            TermColor::Default => if is_fg { (0xCC, 0xCC, 0xCC) } else { (0x1A, 0x1A, 0x2E) },
            TermColor::Rgb(r, g, b) => (r, g, b),
            TermColor::Indexed(i) => ANSI_256[i as usize],
        }
    }
}

// ---------------------------------------------------------------------------
// VTE Handler — processa bytes e alimenta o BlockStore diretamente
// ---------------------------------------------------------------------------

struct VteHandler {
    /// Atributos SGR ativos
    cur_fg: TermColor,
    cur_bg: TermColor,
    cur_bold: bool,
    cur_italic: bool,
    cur_underline: bool,
    /// Referência ao BlockStore para enviar spans
    blocks: BlockStore,
    /// Listener de eventos.
    events: Arc<dyn TerminalEvents>,
    /// Indica que recebemos \r e estamos esperando ver se vem \n logo depois.
    /// Se vier \n → ignora o \r (é só \r\n normal).
    /// Se vier qualquer outra coisa → limpa a linha (autocomplete reescrevendo).
    pending_cr: bool,
}

impl VteHandler {
    fn new(blocks: BlockStore, events: Arc<dyn TerminalEvents>) -> Self {
        Self {
            cur_fg: TermColor::Default,
            cur_bg: TermColor::Default,
            cur_bold: false,
            cur_italic: false,
            cur_underline: false,
            blocks,
            events,
            pending_cr: false,
        }
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.cur_fg = TermColor::Default;
                    self.cur_bg = TermColor::Default;
                    self.cur_bold = false;
                    self.cur_italic = false;
                    self.cur_underline = false;
                }
                1 => self.cur_bold = true,
                3 => self.cur_italic = true,
                4 => self.cur_underline = true,
                22 => self.cur_bold = false,
                23 => self.cur_italic = false,
                24 => self.cur_underline = false,
                n @ 30..=37 => self.cur_fg = TermColor::Indexed(n as u8 - 30),
                38 => {
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        self.cur_fg = TermColor::Indexed(params[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        self.cur_fg = TermColor::Rgb(params[i+2] as u8, params[i+3] as u8, params[i+4] as u8);
                        i += 4;
                    }
                }
                39 => self.cur_fg = TermColor::Default,
                n @ 40..=47 => self.cur_bg = TermColor::Indexed(n as u8 - 40 + 8),
                48 => {
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        self.cur_bg = TermColor::Indexed(params[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        self.cur_bg = TermColor::Rgb(params[i+2] as u8, params[i+3] as u8, params[i+4] as u8);
                        i += 4;
                    }
                }
                49 => self.cur_bg = TermColor::Default,
                n @ 90..=97  => self.cur_fg = TermColor::Indexed(n as u8 - 90 + 8),
                n @ 100..=107 => self.cur_bg = TermColor::Indexed(n as u8 - 100 + 16),
                _ => {}
            }
            i += 1;
        }
    }
}

impl Perform for VteHandler {
    fn print(&mut self, c: char) {
        // Se havia um \r pendente e chegou texto novo → autocomplete reescrevendo a linha
        if self.pending_cr {
            self.pending_cr = false;
            self.blocks.clear_current_line();
        }
        let (r, g, b) = self.cur_fg.to_rgb(true);
        self.blocks.push_span(StyledSpan {
            text: c.to_string(),
            r, g, b,
            bold: self.cur_bold,
            italic: self.cur_italic,
        });
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0B | 0x0C => {
                // \r\n: o \r pendente é ignorado, \n faz o trabalho normalmente
                self.pending_cr = false;
                self.blocks.newline();
            }
            b'\r' => {
                // Marca \r como pendente — só limpa a linha se o próximo byte
                // não for \n (caso do autocomplete reescrevendo a linha)
                if self.pending_cr {
                    // Dois \r seguidos → limpa de qualquer forma
                    self.blocks.clear_current_line();
                }
                self.pending_cr = true;
            }
            0x08 => {
                self.pending_cr = false;
                self.blocks.backspace();
            }
            0x07 => {} // bell
            _ => { self.pending_cr = false; }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        self.pending_cr = false;
        let p: Vec<u16> = params.iter().map(|s| s[0]).collect();
        let p0 = p.first().copied().unwrap_or(0);

        match action {
            // SGR
            'm' => self.apply_sgr(&p),
            // Erase in display (ED) — separação visual
            'J' if p0 == 2 => self.blocks.newline(),
            // Erase in line (EL): CSI 0K ou CSI K — apaga do cursor até fim da linha.
            // Na nossa arquitetura de spans, isso equivale a limpar a linha parcial.
            'K' if p0 == 0 => self.blocks.clear_current_line(),
            // CSI 2K — apaga a linha inteira
            'K' if p0 == 2 => self.blocks.clear_current_line(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 0;title\x07 ou OSC 2;title\x07 → Muda o título da janela/aba
        if (params.len() >= 2) && (params[0] == b"0" || params[0] == b"2") {
            if let Ok(title) = std::str::from_utf8(params[1]) {
                self.events.on_title_changed(title.to_string());
            }
        }
    }
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}

// ---------------------------------------------------------------------------
// TerminalState — wrapper público
// ---------------------------------------------------------------------------

struct Inner {
    parser: Parser,
    handler: VteHandler,
}

#[derive(Clone)]
pub struct TerminalState(Arc<Mutex<Inner>>);

impl TerminalState {
    pub fn new(blocks: BlockStore, events: Arc<dyn TerminalEvents>) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            parser: Parser::new(),
            handler: VteHandler::new(blocks, events),
        })))
    }

    pub fn process_bytes(&self, bytes: &[u8]) {
        let mut inner = self.0.lock().unwrap();
        let Inner { parser, handler } = &mut *inner;
        for &byte in bytes {
            parser.advance(handler, byte);
        }
    }
}

// ---------------------------------------------------------------------------
// Paleta ANSI 256
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const ANSI_256: [(u8, u8, u8); 256] = [
    (0,0,0),(170,0,0),(0,170,0),(170,85,0),(0,0,170),(170,0,170),(0,170,170),(170,170,170),
    (85,85,85),(255,85,85),(85,255,85),(255,255,85),(85,85,255),(255,85,255),(85,255,255),(255,255,255),
    (0,0,0),(0,0,95),(0,0,135),(0,0,175),(0,0,215),(0,0,255),
    (0,95,0),(0,95,95),(0,95,135),(0,95,175),(0,95,215),(0,95,255),
    (0,135,0),(0,135,95),(0,135,135),(0,135,175),(0,135,215),(0,135,255),
    (0,175,0),(0,175,95),(0,175,135),(0,175,175),(0,175,215),(0,175,255),
    (0,215,0),(0,215,95),(0,215,135),(0,215,175),(0,215,215),(0,215,255),
    (0,255,0),(0,255,95),(0,255,135),(0,255,175),(0,255,215),(0,255,255),
    (95,0,0),(95,0,95),(95,0,135),(95,0,175),(95,0,215),(95,0,255),
    (95,95,0),(95,95,95),(95,95,135),(95,95,175),(95,95,215),(95,95,255),
    (95,135,0),(95,135,95),(95,135,135),(95,135,175),(95,135,215),(95,135,255),
    (95,175,0),(95,175,95),(95,175,135),(95,175,175),(95,175,215),(95,175,255),
    (95,215,0),(95,215,95),(95,215,135),(95,215,175),(95,215,215),(95,215,255),
    (95,255,0),(95,255,95),(95,255,135),(95,255,175),(95,255,215),(95,255,255),
    (135,0,0),(135,0,95),(135,0,135),(135,0,175),(135,0,215),(135,0,255),
    (135,95,0),(135,95,95),(135,95,135),(135,95,175),(135,95,215),(135,95,255),
    (135,135,0),(135,135,95),(135,135,135),(135,135,175),(135,135,215),(135,135,255),
    (135,175,0),(135,175,95),(135,175,135),(135,175,175),(135,175,215),(135,175,255),
    (135,215,0),(135,215,95),(135,215,135),(135,215,175),(135,215,215),(135,215,255),
    (135,255,0),(135,255,95),(135,255,135),(135,255,175),(135,255,215),(135,255,255),
    (175,0,0),(175,0,95),(175,0,135),(175,0,175),(175,0,215),(175,0,255),
    (175,95,0),(175,95,95),(175,95,135),(175,95,175),(175,95,215),(175,95,255),
    (175,135,0),(175,135,95),(175,135,135),(175,135,175),(175,135,215),(175,135,255),
    (175,175,0),(175,175,95),(175,175,135),(175,175,175),(175,175,215),(175,175,255),
    (175,215,0),(175,215,95),(175,215,135),(175,215,175),(175,215,215),(175,215,255),
    (175,255,0),(175,255,95),(175,255,135),(175,255,175),(175,255,215),(175,255,255),
    (215,0,0),(215,0,95),(215,0,135),(215,0,175),(215,0,215),(215,0,255),
    (215,95,0),(215,95,95),(215,95,135),(215,95,175),(215,95,215),(215,95,255),
    (215,135,0),(215,135,95),(215,135,135),(215,135,175),(215,135,215),(215,135,255),
    (215,175,0),(215,175,95),(215,175,135),(215,175,175),(215,175,215),(215,175,255),
    (215,215,0),(215,215,95),(215,215,135),(215,215,175),(215,215,215),(215,215,255),
    (215,255,0),(215,255,95),(215,255,135),(215,255,175),(215,255,215),(215,255,255),
    (255,0,0),(255,0,95),(255,0,135),(255,0,175),(255,0,215),(255,0,255),
    (255,95,0),(255,95,95),(255,95,135),(255,95,175),(255,95,215),(255,95,255),
    (255,135,0),(255,135,95),(255,135,135),(255,135,175),(255,135,215),(255,135,255),
    (255,175,0),(255,175,95),(255,175,135),(255,175,175),(255,175,215),(255,175,255),
    (255,215,0),(255,215,95),(255,215,135),(255,215,175),(255,215,215),(255,215,255),
    (255,255,0),(255,255,95),(255,255,135),(255,255,175),(255,255,215),(255,255,255),
    (8,8,8),(18,18,18),(28,28,28),(38,38,38),(48,48,48),(58,58,58),
    (68,68,68),(78,78,78),(88,88,88),(98,98,98),(108,108,108),(118,118,118),
    (128,128,128),(138,138,138),(148,148,148),(158,158,158),(168,168,168),(178,178,178),
    (188,188,188),(198,198,198),(208,208,208),(218,218,218),(228,228,228),(238,238,238),
];

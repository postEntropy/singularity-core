/// main.rs — Singularity Terminal Emulator — Fase 3.5: Multi-sessão
#[cfg(feature = "renderer")]
use singularity_core::session;
use std::sync::Arc;
#[cfg(feature = "renderer")]
use session::Session;
#[cfg(feature = "renderer")]
use anyhow::Result;
#[cfg(feature = "renderer")]
use glyphon::{
    Cache, Color as GColor, FontSystem, Resolution, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
#[cfg(feature = "renderer")]
use session::{BLOCK_PAD_X, BLOCK_PAD_Y, COL_FG, FONT_H, INPUT_BOX_H, MARGIN_X, TAB_BAR_H};
#[cfg(feature = "renderer")]
use wgpu::{
    Color as WColor, CommandEncoderDescriptor, DeviceDescriptor, InstanceDescriptor, LoadOp, MultisampleState,
    Operations, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp,
    SurfaceConfiguration, TextureUsages, TextureViewDescriptor,
};
#[cfg(feature = "renderer")]
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey, ModifiersState},
    window::{Window, WindowId},
};

#[cfg(feature = "renderer")]
// FONT_W é pub para que o LayoutIndex possa calcular colunas corretamente
pub const FONT_W: f32 = 8.4;

#[cfg(feature = "renderer")]
use singularity_core::rect_renderer::{Rect, RectRenderer};

#[cfg(feature = "renderer")]
const BG_MAIN:  WColor = WColor { r: 0.035, g: 0.039, b: 0.05, a: 1.0 }; // Deep Space #090A0D
#[cfg(feature = "renderer")]
// Bloco: Mais escuro, borda leve
const BG_BLOCK: [f32; 4] = [0.08, 0.09, 0.12, 1.0];
#[cfg(feature = "renderer")]
// Input box: Contrastado
const BG_INPUT: [f32; 4] = [0.10, 0.11, 0.15, 1.0];
#[cfg(feature = "renderer")]
// Linha lateral e cursor: Cyan Neon
const ACCENT:   [f32; 4] = [0.0, 0.83, 1.0, 1.0]; // #00D4FF

#[cfg(feature = "renderer")]
// Cores da Tab Bar
const BG_TAB:         [f32; 4] = [0.02, 0.03, 0.04, 1.0];
#[cfg(feature = "renderer")]
const BG_TAB_ACTIVE:  [f32; 4] = [0.08, 0.09, 0.12, 1.0];
#[cfg(feature = "renderer")]
const BORDER_TAB:     [f32; 4] = [0.0, 0.83, 1.0, 0.2]; // ACCENT translúcido

#[cfg(feature = "renderer")]
fn main() -> Result<()> {
    if let Err(e) = env_logger::try_init() {
         eprintln!("Aviso: Falha ao inicializar logger: {}", e);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App { state: None };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(not(feature = "renderer"))]
fn main() {
    println!("Singularity compilado sem a feature 'renderer'. Use --features renderer para a interface gráfica.");
    println!("Consulte WIKI_AGNOSTIC_ENGINE.md para saber como usar o motor como biblioteca.");
}

#[cfg(feature = "renderer")]
struct App { state: Option<AppState> }

#[cfg(feature = "renderer")]
/// Gerencia todas as sessões abertas.
/// Invariante: `sessions` nunca está vazio; `active` é sempre um índice válido.
struct SessionManager {
    sessions: Vec<Session>,
    active: usize,
}

#[cfg(feature = "renderer")]
impl SessionManager {
    fn new(first: Session) -> Self {
        Self { sessions: vec![first], active: 0 }
    }

    fn current(&self) -> &Session {
        &self.sessions[self.active]
    }

    fn current_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active]
    }

    fn add(&mut self, session: Session) {
        self.sessions.push(session);
    }

    /// Alterna para a próxima aba (wrap-around).
    fn next(&mut self) {
        self.active = (self.active + 1) % self.sessions.len();
    }

    /// Alterna para a aba anterior (wrap-around).
    fn prev(&mut self) {
        if self.active == 0 {
            self.active = self.sessions.len() - 1;
        } else {
            self.active -= 1;
        }
    }

    fn count(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(feature = "renderer")]
struct AppState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    rect_renderer: RectRenderer,
    manager: SessionManager,
    modifiers: ModifiersState,
    window: Arc<Window>,
    prompt: String,
    tab_titles_buffer: glyphon::Buffer,
    prompt_buffer: glyphon::Buffer,
}

#[cfg(feature = "renderer")]
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() { return; }
        match init_app(event_loop) {
            Ok(s) => self.state = Some(s),
            Err(e) => { log::error!("{:#}", e); event_loop.exit(); }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ModifiersChanged(mods) => state.modifiers = mods.state(),
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, ..
            } => state.handle_key(logical_key, event_loop),
            WindowEvent::CursorMoved { position, .. } => {
                // Hit-test passivo — apenas loga em debug para validação.
                // Será usado por seleção de texto e hover na Fase 4.
                let session = state.manager.current();
                if let Some(hit) = session.cache.layout.hit_test(
                    position.x as f32,
                    position.y as f32,
                    FONT_W,
                ) {
                    log::debug!("hit: block={} line={} col={}", hit.block_idx, hit.line, hit.col);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = state.render() {
                    match e {
                        wgpu::SurfaceError::Timeout => {
                             // Ignora silenciosamente ou loga em debug — comum em algumas GPUs ao minimizar/trocar aba
                             log::debug!("render timeout: {:#}", e);
                        }
                        wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost => {
                            // Reconfigura a superfície
                            state.resize(PhysicalSize::new(state.surface_config.width, state.surface_config.height));
                        }
                        _ => log::error!("render error: {:#}", e),
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(s) = &self.state { s.window.request_redraw(); }
    }
}

#[cfg(feature = "renderer")]
fn init_app(event_loop: &ActiveEventLoop) -> Result<AppState> {
    let window = std::sync::Arc::new(event_loop.create_window(
        Window::default_attributes()
            .with_title("Singularity")
            .with_inner_size(PhysicalSize::new(1280u32, 720u32)),
    )?);
    let size = window.inner_size();

    let instance = wgpu::Instance::new(&InstanceDescriptor::default());
    let surface = instance.create_surface(window.clone())?;
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .map_err(|e| anyhow::anyhow!("Sem adaptador GPU: {}", e))?;

    let (device, queue) =
        pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))?;

    let caps = surface.get_capabilities(&adapter);
    let fmt = caps.formats.iter().find(|f| f.is_srgb()).copied().unwrap_or(caps.formats[0]);
    let surface_config = SurfaceConfiguration {
        usage: TextureUsages::RENDER_ATTACHMENT,
        format: fmt,
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &surface_config);

    let mut font_system = FontSystem::new();
    let swash_cache = SwashCache::new();
    let cache_gpu = Cache::new(&device);
    let viewport = Viewport::new(&device, &cache_gpu);
    let mut atlas = TextAtlas::new(&device, &queue, &cache_gpu, fmt);
    let text_renderer = TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
    let rect_renderer = RectRenderer::new(&device, fmt);

    let cols = (size.width as f32 / FONT_W) as u16;
    let rows = ((size.height as f32 - INPUT_BOX_H - TAB_BAR_H) / FONT_H) as u16;

    let first_session = Session::new(cols, rows, &mut font_system, "Terminal 1")?;
    let manager = SessionManager::new(first_session);

    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linux".to_string());
    let prompt = format!("{}@{}", user, host);

    let mut tab_titles_buffer = glyphon::Buffer::new(&mut font_system, glyphon::Metrics::new(FONT_H * 0.78, FONT_H));
    tab_titles_buffer.set_size(&mut font_system, Some(size.width as f32), Some(TAB_BAR_H));

    let mut prompt_buffer = glyphon::Buffer::new(&mut font_system, glyphon::Metrics::new(FONT_H * 0.78, FONT_H));
    prompt_buffer.set_size(&mut font_system, Some(size.width as f32), Some(INPUT_BOX_H));
    prompt_buffer.set_text(
        &mut font_system,
        &format!("{} ", prompt),
        &glyphon::Attrs::new().family(glyphon::Family::Monospace).color(GColor::rgb(0x00, 0xFF, 0x9F)),
        glyphon::Shaping::Basic,
        None,
    );
    prompt_buffer.shape_until_scroll(&mut font_system, false);

    Ok(AppState {
        device, queue, surface, surface_config,
        font_system, swash_cache, viewport, atlas, text_renderer,
        rect_renderer,
        manager,
        modifiers: ModifiersState::default(),
        window,
        prompt,
        tab_titles_buffer,
        prompt_buffer,
    })
}

#[cfg(feature = "renderer")]
#[cfg(feature = "renderer")]
impl AppState {
    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 { return; }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);

        self.tab_titles_buffer.set_size(&mut self.font_system, Some(size.width as f32), Some(TAB_BAR_H));
        self.prompt_buffer.set_size(&mut self.font_system, Some(size.width as f32), Some(INPUT_BOX_H));

        let cols = (size.width as f32 / FONT_W) as u16;
        let rows = ((size.height as f32 - INPUT_BOX_H - TAB_BAR_H) / FONT_H) as u16;
        // Redimensiona todas as sessões — processos em background precisam saber o tamanho
        for s in &self.manager.sessions {
            let _ = s.pty.resize(cols, rows);
        }
        // Invalida cache de todas as sessões
        for s in &mut self.manager.sessions {
            s.cache.version = u64::MAX;
        }
    }

    fn handle_key(&mut self, key: Key, _event_loop: &ActiveEventLoop) {
        let ctrl = self.modifiers.control_key();

        // --- Atalhos globais (Ctrl+...) ---
        if ctrl {
            match &key {
                // Ctrl+T — nova aba
                Key::Character(s) if s.as_str() == "t" => {
                    let cols = (self.surface_config.width as f32 / FONT_W) as u16;
                    let rows = ((self.surface_config.height as f32 - INPUT_BOX_H - TAB_BAR_H) / FONT_H) as u16;
                    let n = self.manager.count() + 1;
                    match Session::new(cols, rows, &mut self.font_system, format!("Terminal {}", n)) {
                        Ok(s) => {
                            self.manager.add(s);
                            self.manager.active = self.manager.sessions.len() - 1;
                            log::info!("Nova sessão criada: Terminal {}", n);
                        }
                        Err(e) => log::error!("Falha ao criar sessão: {:#}", e),
                    }
                    return;
                }
                // Ctrl+Tab — próxima aba
                Key::Named(NamedKey::Tab) => {
                    self.manager.next();
                    log::info!("Aba ativa: {}", self.manager.active + 1);
                    return;
                }
                // Ctrl+Shift+Tab — aba anterior (Shift inverte Tab)
                Key::Named(NamedKey::BrowserBack) => {
                    self.manager.prev();
                    return;
                }
                // Ctrl+W — fecha aba atual (mínimo 1)
                Key::Character(s) if s.as_str() == "w" => {
                    if self.manager.count() > 1 {
                        self.manager.sessions.remove(self.manager.active);
                        if self.manager.active >= self.manager.sessions.len() {
                            self.manager.active = self.manager.sessions.len() - 1;
                        }
                        log::info!("Sessão fechada. Abas restantes: {}", self.manager.count());
                    }
                    return;
                }
                // Ctrl+1..9 — pula direto para aba N
                Key::Character(s) => {
                    if let Ok(n) = s.parse::<usize>() {
                        if n >= 1 && n <= self.manager.count() {
                            self.manager.active = n - 1;
                            log::info!("Aba ativa: {}", n);
                        }
                    }
                    return;
                }
                _ => {}
            }
        }

        // --- Input da sessão ativa ---
        let session = self.manager.current_mut();
        match key {
            Key::Named(NamedKey::Enter) => {
                let cmd = session.input_line.clone();
                session.blocks.commit(cmd.clone());
                let _ = session.pty.input_tx.send(format!("{}\r", cmd).into_bytes());
                session.input_line.clear();
            }
            Key::Named(NamedKey::Backspace) => {
                if session.input_line.pop().is_none() {
                    let _ = session.pty.input_tx.send(vec![0x7f]);
                }
            }
            Key::Named(NamedKey::Tab)        => { let _ = session.pty.input_tx.send(vec![b'\t']); }
            Key::Named(NamedKey::Escape)     => { let _ = session.pty.input_tx.send(vec![0x1b]); }
            Key::Named(NamedKey::ArrowUp)    => { let _ = session.pty.input_tx.send(b"\x1b[A".to_vec()); }
            Key::Named(NamedKey::ArrowDown)  => { let _ = session.pty.input_tx.send(b"\x1b[B".to_vec()); }
            Key::Named(NamedKey::ArrowRight) => { let _ = session.pty.input_tx.send(b"\x1b[C".to_vec()); }
            Key::Named(NamedKey::ArrowLeft)  => { let _ = session.pty.input_tx.send(b"\x1b[D".to_vec()); }
            Key::Named(NamedKey::Home)       => { let _ = session.pty.input_tx.send(b"\x1b[H".to_vec()); }
            Key::Named(NamedKey::End)        => { let _ = session.pty.input_tx.send(b"\x1b[F".to_vec()); }
            Key::Named(NamedKey::Delete)     => { let _ = session.pty.input_tx.send(b"\x1b[3~".to_vec()); }
            // winit 0.30 emite espaço como NamedKey::Space, não como Key::Character(" ")
            Key::Named(NamedKey::Space)      => { session.input_line.push(' '); }
            Key::Character(s)               => {
                if !ctrl { session.input_line.push_str(&s); }
            }
            _ => {}
        }
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;
        let scroll_h = h - INPUT_BOX_H;

        let session = self.manager.current_mut();
        let current_version = session.blocks.version();

        if current_version != session.cache.version {
            session.rebuild_buffers(&mut self.font_system, w, scroll_h - TAB_BAR_H);
            session.cache.version = current_version;
        } else {
            session.update_input_buffer(&mut self.font_system, w);
        }

        // --- Atualiza títulos das abas ---
        let mut tab_spans = Vec::new();
        let n_tabs = self.manager.count();
        let tab_w = (w / n_tabs as f32).min(220.0);
        
        for (i, s) in self.manager.sessions.iter().enumerate() {
            let active = i == self.manager.active;
            let color = if active { 
                glyphon::Color::rgb(0x00, 0xD4, 0xFF) // ACCENT
            } else { 
                glyphon::Color::rgb(0x80, 0x80, 0x80) // Gray
            };
            
            // Padding para centralizar o texto na aba (aproximado)
            let title = &s.title;
            let text_len = title.chars().count() as f32 * FONT_W;
            let padding = ((tab_w - text_len) / 2.0).max(0.0);
            let n_spaces = (padding / FONT_W) as usize;
            
            tab_spans.push((" ".repeat(n_spaces), glyphon::Attrs::new()));
            tab_spans.push((title.clone(), glyphon::Attrs::new().color(color).weight(if active { glyphon::Weight::BOLD } else { glyphon::Weight::NORMAL })));
            
            // Completa o resto do espaço da aba com espaços para o próximo título
            let remaining = tab_w - (n_spaces as f32 * FONT_W) - text_len;
            let n_spaces_rem = (remaining / FONT_W) as usize;
            tab_spans.push((" ".repeat(n_spaces_rem), glyphon::Attrs::new()));
        }

        self.tab_titles_buffer.set_size(&mut self.font_system, Some(w), Some(TAB_BAR_H));
        self.tab_titles_buffer.set_rich_text(
            &mut self.font_system,
            tab_spans.iter().map(|(s, a)| (s.as_str(), a.clone())),
            &glyphon::Attrs::new().family(glyphon::Family::Monospace),
            glyphon::Shaping::Basic,
            None,
        );
        self.tab_titles_buffer.shape_until_scroll(&mut self.font_system, false);

        // --- Enfileira retângulos de background ---
        self.rect_renderer.begin_frame();

        // 1. Tab Bar Background
        self.rect_renderer.push(Rect {
            x: 0.0, y: 0.0, w, h: TAB_BAR_H,
            color: BG_TAB,
            radius: 0.0, border_width: 0.0, border_color: [0.0; 4],
        }, w, h);

        // 2. Tabs
        let n_tabs = self.manager.count();
        let tab_w = (w / n_tabs as f32).min(220.0);
        for i in 0..n_tabs {
            let active = i == self.manager.active;
            let color = if active { BG_TAB_ACTIVE } else { BG_TAB };
            let x_start = i as f32 * tab_w;

            self.rect_renderer.push(Rect {
                x: x_start, y: 0.0,
                w: tab_w, h: TAB_BAR_H,
                color,
                radius: 4.0, border_width: 1.0, border_color: BORDER_TAB,
            }, w, h);

            // Se ativa, adiciona underline neon de 2px no fundo da aba
            if active {
                self.rect_renderer.push(Rect {
                    x: x_start + 8.0, y: TAB_BAR_H - 2.0,
                    w: tab_w - 16.0, h: 2.0,
                    color: ACCENT,
                    radius: 1.0, border_width: 0.0, border_color: [0.0; 4],
                }, w, h);
            }
        }

        let session = self.manager.current();

        // Background de cada bloco visível
        for (top, content_h) in session.cache.block_tops.iter()
            .zip(session.cache.block_heights.iter())
        {
            let block_top = TAB_BAR_H + *top;
            if block_top > scroll_h { break; }
            let visible_h = (block_top + content_h).min(scroll_h) - block_top;
            if visible_h <= 0.0 { continue; }

            // Fundo do bloco
            self.rect_renderer.push(Rect {
                x: MARGIN_X, y: block_top,
                w: w - MARGIN_X * 2.0, h: visible_h,
                color: BG_BLOCK,
                radius: 10.0, border_width: 1.0, border_color: [0.1, 0.15, 0.2, 0.5],
            }, w, h);

            // Linha lateral esquerda (accent) — Cyan Neon!
            self.rect_renderer.push(Rect {
                x: MARGIN_X + 1.0, y: block_top + 6.0,
                w: 4.0, h: visible_h - 12.0,
                color: ACCENT,
                radius: 2.0, border_width: 0.0, border_color: [0.0; 4],
            }, w, h);
        }

        // Background do input box - Border Glow (Cyan)
        let input_y = h - INPUT_BOX_H;
        self.rect_renderer.push(Rect {
            x: MARGIN_X, y: input_y + 4.0,
            w: w - MARGIN_X * 2.0, h: INPUT_BOX_H - 8.0,
            color: BG_INPUT,
            radius: 12.0, border_width: 1.5, border_color: ACCENT,
        }, w, h);

        // --- Monta TextAreas ---
        let mut text_areas: Vec<TextArea> = Vec::new();

        for (i, (top, content_h)) in session.cache.block_tops.iter()
            .zip(session.cache.block_heights.iter())
            .enumerate()
        {
            let block_top = TAB_BAR_H + *top;
            if block_top > scroll_h { break; }
            let text_top = block_top + BLOCK_PAD_Y;
            let clip_bottom = (block_top + content_h).min(scroll_h) as i32;
            if clip_bottom <= text_top as i32 { continue; }

            text_areas.push(TextArea {
                buffer: &session.cache.block_buffers[i],
                left: MARGIN_X + BLOCK_PAD_X,
                top: text_top,
                scale: 1.0,
                bounds: TextBounds {
                    left: MARGIN_X as i32,
                    top: text_top as i32,
                    right: (w - MARGIN_X) as i32,
                    bottom: clip_bottom,
                },
                default_color: GColor::rgb(COL_FG.0, COL_FG.1, COL_FG.2),
                custom_glyphs: &[],
            });
        }

        text_areas.push(TextArea {
            buffer: &self.tab_titles_buffer,
            left: 0.0,
            top: (TAB_BAR_H - FONT_H) / 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: TAB_BAR_H as i32,
            },
            default_color: GColor::rgb(0x80, 0x80, 0x80),
            custom_glyphs: &[],
        });

        // Prompt (usuário@host)
        text_areas.push(TextArea {
            buffer: &self.prompt_buffer,
            left: MARGIN_X + BLOCK_PAD_X,
            top: input_y + (INPUT_BOX_H - FONT_H) / 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: (MARGIN_X + BLOCK_PAD_X) as i32,
                top: input_y as i32,
                right: w as i32,
                bottom: h as i32,
            },
            default_color: GColor::rgb(0x00, 0xFF, 0x9F),
            custom_glyphs: &[],
        });

        text_areas.push(TextArea {
            buffer: &session.cache.input_buffer,
            left: MARGIN_X + BLOCK_PAD_X + (self.prompt.chars().count() as f32 + 1.0) * FONT_W,
            top: input_y + (INPUT_BOX_H - FONT_H) / 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: (MARGIN_X + BLOCK_PAD_X) as i32,
                top: input_y as i32,
                right: w as i32,
                bottom: h as i32,
            },
            default_color: GColor::rgb(0xFF, 0xFF, 0xFF),
            custom_glyphs: &[],
        });

        self.viewport.update(&self.queue, Resolution {
            width: self.surface_config.width,
            height: self.surface_config.height,
        });

        self.text_renderer.prepare(
            &self.device, &self.queue, &mut self.font_system,
            &mut self.atlas, &self.viewport,
            text_areas,
            &mut self.swash_cache,
        ).map_err(|_| wgpu::SurfaceError::Lost)?; // glyphon usa Result generic, aqui mapeamos p/ SurfaceError se falhar

        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(
            &CommandEncoderDescriptor { label: None }
        );

        // Cria o vertex buffer ANTES de abrir qualquer render pass
        let rect_vbuf = self.rect_renderer.build_buffer(&self.device);

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("bg_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(BG_MAIN),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(ref vbuf) = rect_vbuf {
                self.rect_renderer.draw(vbuf, &mut pass);
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("text_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load, // preserva o que o bg_pass desenhou
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Pass 2: texto sobre os backgrounds
            self.text_renderer.render(&self.atlas, &self.viewport, &mut pass).map_err(|_| wgpu::SurfaceError::Lost)?;
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.atlas.trim();
        Ok(())
    }
}

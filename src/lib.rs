use std::sync::Arc;

pub mod block_store;
pub mod pty;
pub mod terminal_state;

pub use block_store::{Block, BlockStore, OutputLine, StyledSpan};
pub use pty::{spawn_pty, PtyHandle};
pub use terminal_state::TerminalState;

#[cfg(feature = "renderer")]
pub mod rect_renderer;
#[cfg(feature = "renderer")]
pub mod session;

/// Trait para o motor notificar o mundo exterior sobre mudanças.
pub trait TerminalEvents: Send + Sync {
    /// Chamado quando o buffer de texto é alterado.
    fn on_content_changed(&self);
    /// Chamado quando o PTY solicita redimensionamento ou outras ações.
    fn on_title_changed(&self, title: String);
}

/// No-op implementation para quando não há listener.
pub struct NoopEvents;
impl TerminalEvents for NoopEvents {
    fn on_content_changed(&self) {}
    fn on_title_changed(&self, _title: String) {}
}

/// Helper para criar um Arc<NoopEvents>
pub fn noop_events() -> Arc<dyn TerminalEvents> {
    Arc::new(NoopEvents)
}

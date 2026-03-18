/// block_store.rs — Sistema de blocos estilo Warp.
///
/// # Gerenciamento de Memória
/// O histórico de blocos finalizados é limitado por `MAX_LINES` (total de linhas de output
/// acumuladas). Quando o limite é atingido, blocos inteiros são despejados pela frente
/// (FIFO) até que a contagem de linhas fique abaixo do limite. O despejo opera em O(1)
/// amortizado — remove o bloco mais antigo do `VecDeque` e subtrai sua contagem de linhas.
///
/// Não usamos `VecDeque` de linhas individuais porque despejar por bloco inteiro é mais
/// correto semanticamente (não quebra um bloco no meio) e mais barato (um único pop_front
/// em vez de N pops).
use crate::TerminalEvents;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Limite de linhas totais de output retidas em memória por sessão.
/// 10.000 linhas × ~200 bytes médios por linha ≈ 2MB por sessão — platô aceitável.
pub const MAX_LINES: usize = 10_000;

// ---------------------------------------------------------------------------
// Tipos públicos
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StyledSpan {
    pub text: String,
    pub r: u8, pub g: u8, pub b: u8,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone)]
pub struct OutputLine(pub Vec<StyledSpan>);

impl OutputLine {
    pub fn is_blank(&self) -> bool {
        self.0.iter().all(|s| s.text.chars().all(|c| c == ' '))
    }
}

#[derive(Clone)]
pub struct Block {
    pub command: String,
    pub lines: Vec<OutputLine>,
    pub finished: bool,
    current_line: Vec<StyledSpan>,
}

impl Block {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            lines: Vec::new(),
            finished: false,
            current_line: Vec::new(),
        }
    }

    pub fn push_span(&mut self, span: StyledSpan) {
        self.current_line.push(span);
    }

    pub fn newline(&mut self) {
        let line = OutputLine(std::mem::take(&mut self.current_line));
        self.lines.push(line);
    }

    pub fn trimmed_lines(&self) -> &[OutputLine] {
        let mut end = self.lines.len();
        while end > 0 && self.lines[end - 1].is_blank() {
            end -= 1;
        }
        &self.lines[..end]
    }

    pub fn current_partial(&self) -> &[StyledSpan] {
        &self.current_line
    }

    /// Número de linhas finalizadas (usado para contabilidade do limite de memória).
    fn line_count(&self) -> usize {
        self.lines.len()
    }
}

// ---------------------------------------------------------------------------
// Inner — estado interno protegido por Mutex
// ---------------------------------------------------------------------------

struct Inner {
    /// Histórico de blocos finalizados — VecDeque para despejo O(1) pela frente.
    finished: VecDeque<Block>,
    /// Bloco ativo recebendo output agora.
    active: Block,
    /// Contagem total de linhas em `finished` (não inclui o bloco ativo).
    /// Mantida incrementalmente para evitar O(N) scan no hot path.
    total_lines: usize,
    /// Versão — incrementada a cada mutação para invalidar o BufferCache.
    version: u64,
    /// Listener de eventos.
    events: Arc<dyn TerminalEvents>,
}

impl Inner {
    fn new(events: Arc<dyn TerminalEvents>) -> Self {
        Self {
            finished: VecDeque::new(),
            active: Block::new(""),
            total_lines: 0,
            version: 0,
            events,
        }
    }

    /// Despeja blocos antigos até `total_lines <= MAX_LINES`.
    /// Chamado após cada commit — O(1) amortizado (raramente mais de 1 despejo por commit).
    fn evict_if_needed(&mut self) {
        while self.total_lines > MAX_LINES {
            if let Some(old) = self.finished.pop_front() {
                self.total_lines = self.total_lines.saturating_sub(old.line_count());
            } else {
                break;
            }
        }
    }

    fn commit(&mut self, next_command: impl Into<String>) {
        if !self.active.current_line.is_empty() {
            self.active.newline();
        }
        let mut done = std::mem::replace(&mut self.active, Block::new(next_command));
        done.finished = true;
        let has_content = !done.command.is_empty()
            || done.trimmed_lines().iter().any(|l| !l.is_blank());
        if has_content {
            self.total_lines += done.line_count();
            self.finished.push_back(done);
            self.evict_if_needed();
        }
        self.version += 1;
        self.events.on_content_changed();
    }
}

// ---------------------------------------------------------------------------
// BlockStore — handle público (Arc<Mutex<Inner>>)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BlockStore(Arc<Mutex<Inner>>);

impl BlockStore {
    pub fn new(events: Arc<dyn TerminalEvents>) -> Self {
        Self(Arc::new(Mutex::new(Inner::new(events))))
    }

    pub fn push_span(&self, span: StyledSpan) {
        let mut inner = self.0.lock().unwrap();
        inner.active.push_span(span);
        inner.version += 1;
        inner.events.on_content_changed();
    }

    pub fn newline(&self) {
        let mut inner = self.0.lock().unwrap();
        inner.active.newline();
        inner.version += 1;
        inner.events.on_content_changed();
    }

    /// Apaga o último caractere da linha parcial (backspace).
    pub fn backspace(&self) {
        let mut inner = self.0.lock().unwrap();
        // Remove o último span; se o span tiver mais de 1 char, trunca
        if let Some(last) = inner.active.current_line.last_mut() {
            let mut chars = last.text.chars();
            chars.next_back();
            last.text = chars.as_str().to_string();
            if last.text.is_empty() {
                inner.active.current_line.pop();
            }
        }
        inner.version += 1;
        inner.events.on_content_changed();
    }

    /// Limpa a linha parcial atual (carriage return — shell vai reescrever a linha).
    pub fn clear_current_line(&self) {
        let mut inner = self.0.lock().unwrap();
        inner.active.current_line.clear();
        inner.version += 1;
        inner.events.on_content_changed();
    }

    pub fn commit(&self, next_command: impl Into<String>) {
        self.0.lock().unwrap().commit(next_command);
    }

    pub fn version(&self) -> u64 {
        self.0.lock().unwrap().version
    }

    /// Snapshot para renderização — clona apenas os blocos necessários.
    /// O `VecDeque` é coletado em `Vec` para facilitar iteração no renderer.
    pub fn snapshot(&self) -> (Vec<Block>, Block) {
        let inner = self.0.lock().unwrap();
        (inner.finished.iter().cloned().collect(), inner.active.clone())
    }
}

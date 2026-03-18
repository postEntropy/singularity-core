/// pty.rs — Gerenciamento do pseudo-terminal e comunicação bidirecional.
///
/// Arquitetura de threads:
///   - Thread PTY Reader: lê stdout do PTY em loop tight, envia bytes via crossbeam.
///   - Thread PTY Writer: recebe comandos da UI via canal e escreve no stdin do PTY.
///   - UI Thread: nunca bloqueia em I/O de PTY.
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

/// Tamanho do buffer de leitura do PTY — 64KB é suficiente para bursts de saída
const READ_BUF_SIZE: usize = 65536;

/// Capacidade dos canais — bounded para backpressure natural
const CHANNEL_CAPACITY: usize = 256;

/// Handle para interagir com o PTY a partir da UI.
pub struct PtyHandle {
    /// Envia bytes de input (teclado) para o stdin do PTY
    pub input_tx: Sender<Vec<u8>>,
    /// Recebe bytes de output (stdout/stderr) do PTY — consumido uma vez pela thread de estado
    output_rx: Option<Receiver<Vec<u8>>>,
    /// Permite redimensionar o PTY quando a janela muda de tamanho
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

impl PtyHandle {
    /// Toma o Receiver de output (pode ser chamado apenas uma vez)
    pub fn take_output_rx(&mut self) -> Receiver<Vec<u8>> {
        self.output_rx
            .take()
            .expect("output_rx já foi consumido")
    }

    /// Redimensiona o PTY (chamar quando a janela for redimensionada)
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Falha ao redimensionar PTY")
    }
}

/// Inicializa o PTY e spawna as threads de I/O.
///
/// Retorna um `PtyHandle` que a UI usa para comunicação bidirecional.
pub fn spawn_pty(cols: u16, rows: u16) -> Result<PtyHandle> {
    let pty_system = native_pty_system();

    let pty_pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Falha ao abrir par PTY")?;

    // Detecta o shell do usuário, com fallback para bash
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    log::info!("Iniciando shell: {}", shell);

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Suprime o prompt do shell — a UI tem seu próprio input box com cursor.
    // PS1 vazio evita que o prompt apareça como output no BlockStore.
    cmd.env("PS1", "");
    cmd.env("PS2", "");  // prompt de continuação (ex: após '(')
    cmd.env("PROMPT_COMMAND", ""); // bash executa isso antes de cada PS1

    let _child: Box<dyn Child + Send + Sync> = pty_pair
        .slave
        .spawn_command(cmd)
        .context("Falha ao spawnar processo shell")?;

    // Drop do slave após spawn — o master é suficiente para comunicação
    drop(pty_pair.slave);

    let master = Arc::new(Mutex::new(pty_pair.master));

    // --- Canal UI → PTY (input do teclado) ---
    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(CHANNEL_CAPACITY);

    // --- Canal PTY → UI (output do shell) ---
    let (output_tx, output_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(CHANNEL_CAPACITY);

    // Thread de escrita: recebe input da UI e escreve no stdin do PTY
    {
        let master_write = Arc::clone(&master);
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                let mut writer = master_write.lock().unwrap().take_writer().unwrap();
                drop(master_write);

                for bytes in input_rx {
                    if writer.write_all(&bytes).is_err() {
                        log::warn!("PTY writer: pipe fechado");
                        break;
                    }
                }
                log::info!("Thread pty-writer encerrada");
            })
            .context("Falha ao spawnar thread pty-writer")?;
    }

    // Thread de leitura: lê stdout do PTY e envia para a UI
    {
        let master_read = Arc::clone(&master);
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut reader = master_read.lock().unwrap().try_clone_reader().unwrap();
                drop(master_read);

                let mut buf = vec![0u8; READ_BUF_SIZE];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            log::info!("PTY reader: EOF");
                            break;
                        }
                        Ok(n) => {
                            let chunk = buf[..n].to_vec();
                            if output_tx.send(chunk).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            log::error!("PTY reader erro: {}", e);
                            break;
                        }
                    }
                }
                log::info!("Thread pty-reader encerrada");
            })
            .context("Falha ao spawnar thread pty-reader")?;
    }

    Ok(PtyHandle {
        input_tx,
        output_rx: Some(output_rx),
        master,
    })
}

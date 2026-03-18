# Singularity-Raw: Motor de Terminal Agnóstico

O Singularity-Raw agora é um motor de terminal puro. Ele não sabe desenhar na tela; ele apenas processa o fluxo de dados do PTY através de um parser VTE e mantém um `BlockStore` (histórico estruturado de blocos).

## Como conectar ao Motor

Para usar o motor em seu próprio projeto (ex: um plugin de IDE, uma interface web, ou um renderer 3D):

1. **Implemente a Trait `TerminalEvents`**:
   Esta trait é como o motor fala com você.
   ```rust
   use singularity_core::TerminalEvents;

   struct MyUI;
   impl TerminalEvents for MyUI {
       fn on_content_changed(&self) {
           println!("O buffer de texto mudou! Redesenhe agora.");
       }
       fn on_title_changed(&self, title: String) {
           println!("O shell mudou o título para: {}", title);
       }
   }
   ```

2. **Inicialize o Motor**:
   ```rust
   use singularity_core::{BlockStore, TerminalState, spawn_pty, noop_events};
   use std::sync::Arc;

   // 1. Crie o seu listener
   let events = Arc::new(MyUI);

   // 2. Crie o Store e o Parser ligados a ele
   let blocks = BlockStore::new(events.clone());
   let terminal = TerminalState::new(blocks.clone(), events);

   // 3. Spawne o PTY (cols, rows)
   let mut pty = spawn_pty(80, 24).expect("Falha ao abrir PTY");
   let output_rx = pty.take_output_rx();

   // 4. Conecte o Reader do PTY ao Parser
   std::thread::spawn(move || {
       for chunk in output_rx {
           terminal.process_bytes(&chunk);
       }
   });
   ```

3. **Consuma os Dados**:
   Sempre que `on_content_changed` for chamado, você pode tirar um snapshot do estado:
   ```rust
   let (finished_blocks, active_block) = blocks.snapshot();
   for block in finished_blocks {
       println!("Comando: {}", block.command);
       for line in block.trimmed_lines() {
           // Desenhe as linhas aqui
       }
   }
   ```

4. **Envie Comandos (Input)**:
   Para enviar o que o utilizador digita de volta para o terminal:
   ```rust
   // Envia uma string (ex: comando ls) seguida de Enter (\r)
   pty.input_tx.send(b"ls -la\r".to_vec()).unwrap();
   ```

## Compilação

Se você quer apenas o motor sem as dependências de renderização (`wgpu`, `glyphon`, `winit`), adicione ao seu `Cargo.toml`:

```toml
[dependencies]
singularity_core = { path = "...", default-features = false }
```

A feature `renderer` traz o suporte embutido a GPU se você quiser usá-lo como base para uma UI desktop.

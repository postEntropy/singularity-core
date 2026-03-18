# Singularity — Fase 3.5: Multi-Sessão (Abas)

**Módulos alterados:** `src/main.rs`  
**Módulos criados:** `src/session.rs`  
**Módulos intocados:** `src/pty.rs`, `src/block_store.rs`, `src/terminal_state.rs`  
**Status:** Concluído — compila sem erros ou warnings  
**Data:** 2026-03-17

---

## 1. Problema

O `AppState` original estava acoplado a um único `PtyHandle` e um único `BlockStore`. Para suportar abas, era necessário multiplexar esse estado sem introduzir contenção na UI thread e sem interromper sessões em background.

Três requisitos não-negociáveis guiaram o design:

1. **Sessões em background continuam processando PTY output** — um `cargo build` rodando em outra aba não pode travar.
2. **Alternância de aba é O(1)** — sem reconstrução de buffers ao trocar de aba se o conteúdo não mudou.
3. **Zero deadlock** — a UI thread nunca pode bloquear esperando por uma thread de PTY.

---

## 2. Decisão Arquitetural: `SessionManager` + `Vec<Session>`

A alternativa óbvia seria `Arc<Mutex<Vec<Session>>>` para compartilhar o estado entre threads. Foi descartada: a UI thread seria a única consumidora desse `Vec`, então o `Arc<Mutex>` adicionaria overhead de lock sem nenhum benefício real.

A escolha foi manter o `SessionManager` inteiramente na UI thread — um `Vec<Session>` simples com um índice `active: usize`. Sem lock, sem overhead, sem risco de deadlock entre UI e PTY.

O isolamento real já existia na camada abaixo: cada `Session` spawna suas próprias threads `pty-reader`, `pty-writer` e `vte-processor`, que se comunicam com a UI exclusivamente via `crossbeam_channel` bounded e `Arc<Mutex<Inner>>` do `BlockStore`. Essas threads são completamente independentes entre si e da UI thread.

```
UI Thread
└── SessionManager { sessions: Vec<Session>, active: usize }
    ├── Session[0]  ←→  pty-reader[0] / pty-writer[0] / vte-processor[0]
    ├── Session[1]  ←→  pty-reader[1] / pty-writer[1] / vte-processor[1]
    └── Session[N]  ←→  pty-reader[N] / pty-writer[N] / vte-processor[N]
```

Cada grupo de threads de PTY opera de forma completamente independente. Quando a aba 1 está ativa, as threads da aba 2 continuam rodando — alimentando o `BlockStore` da aba 2 via `push_span/newline` sem nenhuma interação com a UI.

---

## 3. Novo Módulo: `session.rs`

Toda a lógica que antes estava espalhada no `AppState` foi encapsulada em `Session`:

```rust
pub struct Session {
    pub pty: PtyHandle,
    pub blocks: BlockStore,
    pub terminal: TerminalState,
    pub input_line: String,
    pub cache: BufferCache,
    pub title: String,
}
```

`Session::new(cols, rows, font_system, title)` é o único ponto de criação — spawna o PTY, inicializa o parser VTE, spawna a thread `vte-processor` e inicializa o `BufferCache`. Criar uma nova aba é uma única chamada.

### 3.1 `BufferCache` por Sessão

Cada `Session` carrega seu próprio `BufferCache`:

```rust
pub struct BufferCache {
    pub version: u64,
    pub block_buffers: Vec<GlyphonBuffer>,
    pub input_buffer: GlyphonBuffer,
    pub block_tops: Vec<f32>,
    pub block_heights: Vec<f32>,
}
```

O mecanismo de invalidação por versão da Fase 3 foi preservado integralmente. Quando o usuário alterna para uma aba que recebeu output em background, o cache dessa aba tem `version != blocks.version()` — o `rebuild_buffers()` é chamado uma única vez no próximo frame. Se a aba não recebeu nenhum output enquanto estava em background, o cache está válido e a alternância é literalmente O(1): apenas troca o ponteiro `active`.

### 3.2 Métodos de renderização movidos para `Session`

`rebuild_buffers()` e `update_input_buffer()` foram movidos de `AppState` para `Session`, recebendo `&mut FontSystem` como parâmetro. O `FontSystem` continua sendo único e compartilhado (vive em `AppState`) — isso é correto porque o shaping de texto não é thread-safe e ocorre exclusivamente na UI thread.

---

## 4. `SessionManager`

```rust
struct SessionManager {
    sessions: Vec<Session>,
    active: usize,
}
```

Invariante mantida em todos os métodos: `sessions` nunca está vazio e `active` é sempre um índice válido.

| Método | Comportamento |
|---|---|
| `current() / current_mut()` | Acesso à sessão ativa |
| `add(session)` | Appenda nova sessão |
| `next()` | Avança `active` com wrap-around |
| `prev()` | Recua `active` com wrap-around |
| `count()` | Número de sessões abertas |

---

## 5. Atalhos de Teclado

Implementados via detecção de `Modifiers` no `WindowEvent::ModifiersChanged` — o estado de `Ctrl` é mantido em `AppState::modifiers: Modifiers` e consultado em cada `KeyboardInput`.

| Atalho | Ação |
|---|---|
| `Ctrl+T` | Cria nova sessão PTY e ativa imediatamente |
| `Ctrl+Tab` | Alterna para a próxima aba (wrap-around) |
| `Ctrl+W` | Fecha a aba ativa (mínimo de 1 aba mantida) |
| `Ctrl+1..9` | Pula diretamente para a aba N |

Quando `Ctrl` está pressionado, os atalhos globais são processados antes de qualquer input chegar à sessão ativa — evita que `Ctrl+T` insira um `t` no terminal.

### Detalhe: `Ctrl+W` e limpeza de sessão

Ao fechar uma aba via `Ctrl+W`, a `Session` é removida do `Vec` via `sessions.remove(active)`. As threads de PTY associadas (`pty-reader`, `pty-writer`, `vte-processor`) encerram naturalmente: o `Sender` do canal `input_tx` é dropado junto com o `PtyHandle`, o que causa `RecvError` na thread `pty-writer` e encerramento limpo. O `pty-reader` encerra ao receber EOF do master PTY quando o processo shell termina.

---

## 6. Resize com Múltiplas Sessões

`AppState::resize()` agora itera sobre **todas** as sessões para enviar `SIGWINCH`:

```rust
for s in &self.manager.sessions {
    let _ = s.pty.resize(cols, rows);
}
for s in &mut self.manager.sessions {
    s.cache.version = u64::MAX; // invalida cache de todas as abas
}
```

Sessões em background recebem o resize corretamente — processos como `vim` ou `htop` rodando em abas inativas se adaptam ao novo tamanho sem precisar estar em foco.

---

## 7. Estrutura de Arquivos Atualizada

```
singularity/
├── Cargo.toml
└── src/
    ├── main.rs           — event loop, SessionManager, renderer wgpu/glyphon
    ├── session.rs        — Session, BufferCache, rebuild_buffers (NOVO)
    ├── pty.rs            — PTY setup, threads reader/writer, PtyHandle
    ├── terminal_state.rs — parser VTE, VteHandler, TermColor, paleta ANSI 256
    └── block_store.rs    — BlockStore, Block, OutputLine, StyledSpan
```

---

## 8. O que NÃO foi feito (intencionalmente)

- **UI de abas (tab bar):** nenhum elemento visual de abas foi adicionado — a fundação de dados está pronta, a renderização da barra de abas é trabalho da Fase 4 (design visual).
- **Persistência de sessão:** sessões fechadas são descartadas sem salvar histórico — feature de roadmap futuro.
- **Limite de sessões:** não há limite imposto por código — o limite prático é memória disponível (cada sessão usa ~3 threads + heap do BlockStore).

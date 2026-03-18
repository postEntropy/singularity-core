# Singularity Terminal Emulator — Relatório Técnico de Desenvolvimento

**Projeto:** Singularity  
**Linguagem:** Rust (Edition 2021)  
**Plataforma alvo:** Linux (Fedora/Wayland)  
**Status:** Fase 3.5 / Pré-Fase 4 concluída — multi-sessão, gerenciamento de memória e hit-testing  
**Data:** 2026-03-17

---

## 1. Visão Geral e Motivação

O Singularity é um emulador de terminal moderno inspirado na arquitetura de blocos do Warp. A premissa central é que cada comando executado pelo usuário gera um **bloco independente** — uma unidade visual que agrupa o comando e seu output, permitindo navegação, seleção e futuramente colapso/expansão de resultados.

A prioridade absoluta do projeto é **performance bruta e baixo consumo de recursos**: zero alocações desnecessárias no loop de renderização, latência de input sub-milissegundo e throughput de I/O do PTY sem bloqueio da UI thread.

---

## 2. Decisões Arquiteturais de Stack

### 2.1 Por que não `gpui` (Zed Industries)

A stack original sugerida incluía `gpui` como framework de UI. Foi descartado pelos seguintes motivos:

- API pública instável — sem garantias de compatibilidade entre versões
- Fortemente acoplado ao ecossistema interno do Zed (macOS-first)
- Sem suporte documentado para Wayland nativo no Linux
- Overhead de abstração desnecessário para um terminal

### 2.2 Stack adotada

| Componente | Crate | Versão | Justificativa |
|---|---|---|---|
| Janela + eventos | `winit` | 0.30 | Suporte nativo Wayland via `xdg-shell`, API `ApplicationHandler` estável |
| Pipeline GPU | `wgpu` | 28 | Backend Vulkan/OpenGL, cross-platform, zero unsafe exposto |
| Renderização de texto | `glyphon` | 0.10 | Text rendering GPU-accelerated sobre wgpu via `cosmic-text` (shaping correto, suporte a Unicode) |
| PTY | `portable-pty` | 0.8 | Battle-tested, suporte a resize SIGWINCH, abstração cross-platform |
| Canais inter-thread | `crossbeam-channel` | 0.5 | Canais bounded lock-free, backpressure natural, zero overhead vs `std::sync::mpsc` |
| Parser VT/ANSI | `vte` | 0.13 | Parser de estado finito baseado no DEC ANSI parser, zero-copy, callbacks síncronos |
| Erros | `anyhow` | 1 | Ergonomia de error handling sem overhead de runtime |
| Async init | `pollster` | 0.4 | Block-on para inicialização síncrona do wgpu sem tokio runtime |

### 2.3 Perfil de release

```toml
[profile.release]
lto = "fat"          # Link-Time Optimization completo entre todos os crates
codegen-units = 1    # Compilação em unidade única — máxima otimização inter-procedural
opt-level = 3        # Otimização máxima
strip = true         # Remove símbolos de debug do binário final
```

---

## 3. Arquitetura de Threads

O isolamento de I/O é o requisito mais crítico: o event loop da UI **nunca pode bloquear** em operações de PTY.

```
┌─────────────────────────────────────────────────────────────┐
│                        UI Thread                            │
│  winit event loop → handle_key → render()                   │
│  Lê: BlockStore (snapshot lock ~microsegundos)              │
│  Escreve: input_tx (send não-bloqueante)                    │
└──────────────────────┬──────────────────────────────────────┘
                       │ crossbeam bounded(256)
                       ▼
┌──────────────────────────────────────────────────────────────┐
│                   Thread: pty-writer                         │
│  Drena input_rx → write_all() no stdin do PTY               │
└──────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────┐
│                   Thread: pty-reader                         │
│  read() bloqueante no master PTY (buf 64KB)                 │
│  → output_tx.send(chunk)                                    │
└──────────────────────┬───────────────────────────────────────┘
                       │ crossbeam bounded(256)
                       ▼
┌──────────────────────────────────────────────────────────────┐
│                  Thread: vte-processor                       │
│  Drena output_rx → vte::Parser::advance()                   │
│  → VteHandler::print/execute/csi_dispatch                   │
│  → BlockStore::push_span / newline                          │
└──────────────────────────────────────────────────────────────┘
```

**Canais bounded:** a capacidade de 256 chunks por canal garante backpressure natural — se o renderer não conseguir acompanhar o output do PTY, a thread de leitura bloqueia no `send()` em vez de crescer a memória indefinidamente.

---

## 4. Módulos

### 4.1 `pty.rs` — Gerenciamento do Pseudo-Terminal

Responsável por:

1. Abrir o par PTY via `portable_pty::native_pty_system().openpty()`
2. Detectar o shell do usuário via `$SHELL` (fallback: `/bin/bash`)
3. Spawnar o processo shell no slave PTY com `TERM=xterm-256color` e `COLORTERM=truecolor`
4. Dropar o slave após o spawn (o master é suficiente para comunicação bidirecional)
5. Spawnar as threads `pty-reader` e `pty-writer`

**`PtyHandle`** expõe:
- `input_tx: Sender<Vec<u8>>` — canal para enviar bytes de teclado ao PTY
- `take_output_rx() -> Receiver<Vec<u8>>` — consumível uma única vez pela thread VTE
- `resize(cols, rows)` — envia `SIGWINCH` via `MasterPty::resize()`

O `output_rx` é encapsulado em `Option<Receiver>` para garantir que seja consumido exatamente uma vez — tentativas subsequentes de `take_output_rx()` causam panic explícito, evitando bugs silenciosos de múltiplos consumidores.

### 4.2 `terminal_state.rs` — Parser VTE

Implementa `vte::Perform` no struct `VteHandler`, que mantém o estado SGR (Select Graphic Rendition) atual e alimenta o `BlockStore` diretamente via callbacks.

**Estado SGR mantido:**
- `cur_fg / cur_bg: TermColor` — cor atual de foreground/background
- `cur_bold / cur_italic / cur_underline: bool` — atributos de texto

**`TermColor`** suporta três modos:
```rust
pub enum TermColor {
    Default,           // cor padrão do tema
    Indexed(u8),       // paleta ANSI 0-255
    Rgb(u8, u8, u8),   // true-color (ESC[38;2;r;g;bm)
}
```

A paleta de 256 cores é uma tabela estática `const ANSI_256: [(u8,u8,u8); 256]` — zero alocação em runtime.

**Callbacks implementados:**

| Callback | Ação |
|---|---|
| `print(char)` | Cria `StyledSpan` com cor/atributos atuais → `BlockStore::push_span()` |
| `execute(0x0A/0x0B/0x0C)` | `BlockStore::newline()` |
| `csi_dispatch('m', ...)` | `apply_sgr()` — atualiza estado de cor |
| `csi_dispatch('J', 2)` | Erase display → emite newline para separação visual |

**Decisão de design:** o `VteHandler` não mantém uma grade 2D de células. Em vez disso, alimenta o `BlockStore` diretamente com spans de texto. Isso elimina a necessidade de sincronizar snapshots da grade com os blocos (que era a causa raiz do bug de duplicação de output nas fases anteriores).

### 4.3 `block_store.rs` — Sistema de Blocos

É o coração da arquitetura Warp-like. Gerencia a lista de blocos e é o único ponto de estado compartilhado entre a thread VTE e a UI thread.

**Hierarquia de tipos:**

```
BlockStore (Arc<Mutex<Inner>>)
└── Inner
    ├── finished: VecDeque<Block>  — blocos commitados (despejo FIFO pelo front)
    ├── active: Block              — bloco atual recebendo output
    ├── total_lines: usize         — contador incremental de linhas (sem scan O(N))
    └── version: u64               — contador de mudanças

Block
├── command: String            — texto do comando que originou o bloco
├── lines: Vec<OutputLine>     — linhas finalizadas (com \n)
├── current_line: Vec<StyledSpan>  — linha parcial em construção
└── finished: bool

OutputLine(Vec<StyledSpan>)

StyledSpan
├── text: String
├── r, g, b: u8
├── bold: bool
└── italic: bool
```

**Fluxo de dados:**

1. `VteHandler::print(c)` → `BlockStore::push_span()` — appenda span à `current_line` do bloco ativo
2. `VteHandler::execute('\n')` → `BlockStore::newline()` — move `current_line` para `lines` via `std::mem::take()`
3. `AppState::handle_key(Enter)` → `BlockStore::commit(cmd)` — finaliza linha parcial, move bloco ativo para `finished`, cria novo bloco ativo com o próximo comando

**Sistema de versão:** cada mutação do `BlockStore` incrementa `Inner::version: u64`. O renderer compara `blocks.version()` com `cache.version` antes de reconstruir os buffers de texto — se a versão não mudou, o frame é renderizado com os buffers cacheados sem nenhuma alocação.

**`trimmed_lines()`:** remove trailing blank lines do output antes da renderização, evitando espaço vazio desnecessário no final de cada bloco.

### 4.4 `session.rs` — Sessão PTY (criado na Fase 3.5)

Encapsula todo o estado de uma aba: `PtyHandle`, `BlockStore`, `TerminalState`, `BufferCache` e `input_line`. Extraído do `AppState` para permitir múltiplas instâncias independentes.

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

`Session::new()` é o único ponto de criação — spawna o PTY, inicializa o parser VTE, spawna a thread `vte-processor` e inicializa o `BufferCache`. Criar uma nova aba é uma única chamada.

O `BufferCache` agora inclui também o `LayoutIndex` (ver seção 7).

### 4.5 `main.rs` — Event Loop e Renderer

#### Inicialização wgpu

Segue o padrão canônico do glyphon:

```
Instance → Surface → Adapter (HighPerformance) → Device + Queue
→ SurfaceConfiguration (sRGB, Fifo/vsync)
→ FontSystem + SwashCache + Cache + Viewport + TextAtlas + TextRenderer
```

O `pollster::block_on()` é usado para inicialização síncrona dentro do `ApplicationHandler::resumed()` — evita a necessidade de um tokio runtime apenas para setup.

#### `BufferCache` — Invalidação por Versão

```rust
struct BufferCache {
    version: u64,
    block_buffers: Vec<GlyphonBuffer>,
    input_buffer: GlyphonBuffer,
    block_tops: Vec<f32>,
    block_heights: Vec<f32>,
}
```

O cache é invalidado (e reconstruído via `rebuild_buffers()`) apenas quando `blocks.version() != cache.version`. Isso garante que frames sem mudança de conteúdo não fazem nenhuma alocação de heap — apenas montam `TextArea` com referências aos buffers existentes e chamam `text_renderer.prepare()`.

#### `rebuild_buffers()` — Construção dos Buffers de Texto

Para cada bloco visível:

1. Calcula `content_h = n_lines * FONT_H + BLOCK_PAD_Y * 2.0`
2. Constrói `Vec<(String, Attrs)>` com spans coloridos
3. Cria `GlyphonBuffer`, chama `set_rich_text()` com os spans
4. Chama `shape_until_scroll()` para shaping via cosmic-text
5. Armazena buffer + posição Y no cache

A linha parcial (`current_partial()`) é incluída no último bloco para exibir output em tempo real antes do `\n`.

#### Loop de Renderização

```
render()
├── Verifica versão → rebuild_buffers() se necessário
├── Monta Vec<TextArea> com referências aos buffers cacheados
│   ├── Blocos: clipping por scroll_h (h - INPUT_BOX_H)
│   └── Input box: sempre visível, posição fixa em h - INPUT_BOX_H
├── viewport.update()
├── text_renderer.prepare() — rasteriza glifos no atlas GPU
├── begin_render_pass (LoadOp::Clear BG_MAIN)
├── text_renderer.render()
└── queue.submit() + frame.present() + atlas.trim()
```

O `atlas.trim()` ao final de cada frame libera glifos não utilizados do atlas de textura GPU, evitando crescimento ilimitado de VRAM.

#### Input Box

O input é gerenciado inteiramente na UI thread — `input_line: String` acumula os caracteres digitados. Ao pressionar Enter:

1. `blocks.commit(cmd)` — congela o bloco ativo
2. `pty.input_tx.send(format!("{}\r", cmd))` — envia o comando ao PTY
3. `input_line.clear()` — limpa o campo

O `\r` (carriage return) é enviado em vez de `\n` porque o PTY opera em modo raw e espera CR para processar o comando.

---

## 5. Fase 3.5 — Multi-Sessão (Abas)

### 5.1 Decisão Arquitetural: `SessionManager` + `Vec<Session>`

A alternativa óbvia seria `Arc<Mutex<Vec<Session>>>` para compartilhar o estado entre threads. Foi descartada: a UI thread é a única consumidora desse `Vec`, então o `Arc<Mutex>` adicionaria overhead de lock sem nenhum benefício real.

O `SessionManager` vive inteiramente na UI thread — um `Vec<Session>` simples com um índice `active: usize`. Sem lock, sem overhead, sem risco de deadlock entre UI e PTY.

```
UI Thread
└── SessionManager { sessions: Vec<Session>, active: usize }
    ├── Session[0]  ←→  pty-reader[0] / pty-writer[0] / vte-processor[0]
    ├── Session[1]  ←→  pty-reader[1] / pty-writer[1] / vte-processor[1]
    └── Session[N]  ←→  pty-reader[N] / pty-writer[N] / vte-processor[N]
```

O isolamento real já existia na camada abaixo: cada `Session` spawna suas próprias threads `pty-reader`, `pty-writer` e `vte-processor`, que se comunicam com a UI exclusivamente via `crossbeam_channel` bounded e `Arc<Mutex<Inner>>` do `BlockStore`. Sessões em background continuam processando PTY output normalmente — um `cargo build` rodando em outra aba não trava.

### 5.2 `SessionManager`

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
| `next() / prev()` | Alterna com wrap-around |
| `count()` | Número de sessões abertas |

### 5.3 `BufferCache` por Sessão e Alternância O(1)

Cada `Session` carrega seu próprio `BufferCache`. Quando o usuário alterna para uma aba que recebeu output em background, o cache dessa aba tem `version != blocks.version()` — o `rebuild_buffers()` é chamado uma única vez no próximo frame. Se a aba não recebeu output enquanto estava em background, o cache está válido e a alternância é O(1): apenas troca o índice `active`.

### 5.4 Atalhos de Teclado

| Atalho | Ação |
|---|---|
| `Ctrl+T` | Cria nova sessão PTY e ativa imediatamente |
| `Ctrl+Tab` | Próxima aba (wrap-around) |
| `Ctrl+W` | Fecha aba ativa (mínimo de 1 mantida) |
| `Ctrl+1..9` | Pula diretamente para aba N |

Ao fechar uma aba via `Ctrl+W`, as threads de PTY associadas encerram naturalmente: o `Sender` do canal `input_tx` é dropado junto com o `PtyHandle`, causando `RecvError` na thread `pty-writer`. O `pty-reader` encerra ao receber EOF do master PTY quando o processo shell termina.

### 5.5 Resize com Múltiplas Sessões

`AppState::resize()` itera sobre todas as sessões para enviar `SIGWINCH` — processos como `vim` ou `htop` rodando em abas inativas se adaptam ao novo tamanho sem precisar estar em foco.

---

## 6. Pré-Fase 4 — Gerenciamento de Memória e Hit-Testing

### 6.1 Problema I: Crescimento Ilimitado de Memória

O `Inner` do `BlockStore` usava `Vec<Block>` sem teto de capacidade. Em sessões com output contínuo (`tail -f`, `cargo build` de projetos grandes), esse vetor cresce linearmente sem limite — tanto em heap quanto no `BufferCache` de glifos na VRAM.

**Solução: `VecDeque` + contador incremental de linhas**

```rust
struct Inner {
    finished: VecDeque<Block>,  // era Vec<Block>
    active: Block,
    total_lines: usize,         // mantido incrementalmente — zero scan O(N)
    version: u64,
}
```

O limite é definido por número de linhas totais (não por número de blocos):

```rust
pub const MAX_LINES: usize = 10_000;
```

Linhas são a unidade correta porque um único bloco pode ter 1 linha (`echo ok`) ou 5.000 linhas (`cargo test`). O despejo ocorre em `commit()` via `pop_front()` O(1) amortizado — remove o bloco mais antigo inteiro, preservando integridade semântica (não quebra um bloco no meio).

| Cenário | Antes | Depois |
|---|---|---|
| Sessão idle | ~KB | ~KB |
| `tail -f` por 1h | cresce indefinidamente | platô em ~2MB |
| `cargo build` grande | pico de dezenas de MB | platô em ~2MB |
| 10 abas com output contínuo | N × ∞ | N × ~2MB |

**Por que não ring buffer manual?** Blocos têm tamanho variável (1 a milhares de linhas) — um ring buffer de capacidade fixa exige elementos de tamanho uniforme. `VecDeque` resolve com `pop_front()` O(1) sem gerenciar índices `head/tail` manualmente.

### 6.2 Problema II: Mapeamento Espacial (Hit-Testing)

O `glyphon` não expõe API de hit-testing reverso. Sem mapeamento entre pixels e estado lógico, é impossível implementar seleção de texto, hover ou colapso de blocos.

**Solução: `LayoutIndex` com busca binária**

```rust
pub struct BlockRect { pub y0: f32, pub y1: f32, pub x0: f32 }

pub struct LayoutIndex {
    pub rects: Vec<BlockRect>,  // um por bloco, ordenado por y0 crescente
}
```

O `LayoutIndex` vive dentro do `BufferCache` e é reconstruído junto com os `GlyphonBuffer` — sempre sincronizado, zero estado desincronizado possível.

**API:**

```rust
pub fn hit_test(&self, x: f32, y: f32, font_w: f32) -> Option<HitResult>
// Retorna: HitResult { block_idx, line, col }
```

**Algoritmo:**
```
1. partition_point(|r| r.y0 <= y)  →  O(log k), k = blocos visíveis
2. Verifica y ∈ [y0, y1)           →  O(1)
3. line = (y - y0 - BLOCK_PAD_Y) / FONT_H  →  O(1)
4. col  = (x - x0) / font_w                →  O(1)
```

`partition_point` é a busca binária da stdlib sobre um `Vec` contíguo — excelente localidade de cache, O(log 50) ≈ 6 comparações para k típico. Uma interval tree adicionaria complexidade sem ganho mensurável para k < 50.

O `CursorMoved` do winit chama o hit-test passivamente e loga o resultado em `debug` — infraestrutura pronta para hover, seleção e colapso na Fase 4.

---

## 7. Bugs Resolvidos Durante o Desenvolvimento

### 5.1 Duplicação de Output (Fase 3)

**Causa:** A abordagem inicial sincronizava o bloco ativo com snapshots da grade VTE (`update_from_grid`). Quando um bloco era commitado, a grade continuava sendo atualizada pela thread VTE e o conteúdo aparecia tanto no bloco finalizado quanto no novo bloco ativo.

**Solução:** Eliminação completa da grade 2D. O `VteHandler` agora alimenta o `BlockStore` diretamente via callbacks `push_span/newline`. Cada bloco acumula seu próprio output de forma independente — não há estado compartilhado entre blocos.

### 5.2 Timeout no Renderer (Fase 3)

**Causa:** A implementação anterior criava um `GlyphonBuffer` novo por bloco a cada frame, mesmo sem mudança de conteúdo. O shaping de texto via cosmic-text é custoso — com múltiplos blocos, o tempo de frame excedia o timeout de aquisição do swapchain.

**Solução:** `BufferCache` com invalidação por versão. Buffers são reconstruídos apenas quando `BlockStore::version` muda. Frames sem mudança de conteúdo têm custo O(n_blocos_visíveis) apenas para montar as `TextArea` — sem alocações de heap.

### 5.3 Incompatibilidades de API wgpu 28 / glyphon 0.10

- `request_adapter()` retorna `Result` em wgpu 28 (não `Option`) — uso de `.map_err()` em vez de `.ok_or_else()`
- `RenderPassColorAttachment` tem campo `depth_slice: None` adicionado em wgpu 28
- `RenderPassDescriptor` tem campo `multiview_mask: None` adicionado em wgpu 28
- `Buffer::set_text()` e `set_rich_text()` recebem `&Attrs` (referência) em cosmic-text 0.15
- `set_rich_text()` aceita `IntoIterator<Item = (&str, Attrs)>` — não `(text, AttrsList)`
- `DeviceDescriptor::request_device()` não tem segundo argumento `None` em wgpu 28

---

## 8. Estrutura de Arquivos

```
singularity-core/
├── Cargo.toml
└── src/
    ├── main.rs           — event loop winit, SessionManager, renderer wgpu/glyphon, CursorMoved hit-test
    ├── session.rs        — Session, BufferCache, LayoutIndex, BlockRect, HitResult, rebuild_buffers
    ├── pty.rs            — PTY setup, threads reader/writer, PtyHandle
    ├── terminal_state.rs — parser VTE, VteHandler, TermColor, paleta ANSI 256
    └── block_store.rs    — BlockStore, VecDeque, MAX_LINES, evict_if_needed
```

---

## 9. Roadmap — Próximas Fases

### Fase 4 — Visual Warp-like
- Backgrounds por bloco (`#2C2C2E`) via wgpu render pass separado, usando `BlockRect.y0/y1` do `LayoutIndex` diretamente
- Hover highlight — `HitResult.block_idx` identifica o bloco sob o cursor para mudar cor de fundo
- Bordas e separadores visuais entre blocos
- Barra de abas (tab bar) renderizada no topo

### Fase 5 — UX
- Scroll do histórico de blocos (mouse wheel + teclado)
- Seleção de texto — `LayoutIndex` identifica o bloco, `Buffer::hit` do glyphon resolve o glifo exato
- Colapso/expansão de blocos — `collapsed: bool` por bloco, `LayoutIndex` reflete altura reduzida
- Ctrl+C para SIGINT no processo filho

### Fase 6 — Features Avançadas
- Syntax highlighting do comando no input box (via `tree-sitter`)
- Autocompletion integrado
- Busca no histórico de blocos

---

## 10. Como Compilar e Executar

```bash
# Dev (sem otimizações, com debug info)
cargo run

# Release (LTO + opt-level 3)
cargo build --release
./target/release/singularity-core

# Variável de ambiente para logs detalhados
RUST_LOG=info cargo run
```

**Dependências de sistema (Fedora):**
```bash
sudo dnf install vulkan-loader-devel libxkbcommon-devel wayland-devel
```

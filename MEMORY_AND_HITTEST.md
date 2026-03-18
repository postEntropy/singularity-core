# Singularity — Fase 4 (Pré): Gerenciamento de Memória e Hit-Testing

**Módulos alterados:** `src/block_store.rs`, `src/session.rs`, `src/main.rs`  
**Status:** Concluído — compila sem erros ou warnings  
**Data:** 2026-03-17

---

## 1. Contexto

Antes de implementar a UI visual estilo Warp (backgrounds por bloco, bordas, seleção de texto), dois gargalos estruturais precisavam ser resolvidos:

1. O `BlockStore` crescia ilimitadamente — `Vec<Block>` sem teto de capacidade, O(N) de memória onde N → ∞ em sessões longas.
2. Não havia mapeamento entre pixels renderizados e estado lógico — impossível saber qual bloco/linha/coluna o cursor do mouse estava tocando.

Ambos foram resolvidos sem introduzir nenhuma crate nova.

---

## 2. Problema I: Crescimento Ilimitado de Memória

### Causa raiz

O `Inner` do `BlockStore` usava `Vec<Block>` para o histórico de blocos finalizados. Cada `Block` acumula `Vec<OutputLine>`, cada `OutputLine` acumula `Vec<StyledSpan>`, cada `StyledSpan` carrega uma `String`. Em sessões com output contínuo (logs de servidor, `cargo build` de projetos grandes), esse vetor cresce linearmente sem limite — tanto em heap quanto no `BufferCache` de glifos na VRAM.

### Solução: `VecDeque` + contador incremental de linhas

```rust
struct Inner {
    finished: VecDeque<Block>,   // era Vec<Block>
    active: Block,
    total_lines: usize,          // novo — mantido incrementalmente
    version: u64,
}
```

A troca de `Vec` por `VecDeque` habilita `pop_front()` O(1) amortizado — remoção do bloco mais antigo sem realocação ou shift de elementos.

O limite é definido por número de **linhas totais**, não por número de blocos:

```rust
pub const MAX_LINES: usize = 10_000;
```

Linhas são a unidade correta porque um único bloco pode ter 1 linha (`echo ok`) ou 5.000 linhas (`cargo test` de um projeto grande). Limitar por número de blocos seria impreciso.

### Estratégia de despejo

O despejo ocorre em `commit()`, após cada novo bloco ser adicionado:

```rust
fn evict_if_needed(&mut self) {
    while self.total_lines > MAX_LINES {
        if let Some(old) = self.finished.pop_front() {
            self.total_lines = self.total_lines.saturating_sub(old.line_count());
        } else {
            break;
        }
    }
}
```

**Por que despejar por bloco inteiro e não por linha individual?**

Despejar linhas individuais quebraria blocos no meio — o renderer receberia um bloco com comando mas sem as primeiras N linhas de output, o que é semanticamente incorreto e visualmente confuso. Despejar o bloco mais antigo inteiro é O(1) e mantém a integridade semântica de cada bloco.

**Por que não um ring buffer manual?**

Um ring buffer circular de capacidade fixa exige que todos os elementos tenham tamanho uniforme. Blocos têm tamanho variável (1 a milhares de linhas). `VecDeque` resolve o mesmo problema com `pop_front()` O(1) sem a complexidade de gerenciar índices `head/tail` manualmente.

### Comportamento de memória resultante

| Cenário | Antes | Depois |
|---|---|---|
| Sessão idle (poucos comandos) | ~KB | ~KB (sem diferença) |
| `tail -f /var/log/syslog` por 1h | cresce indefinidamente | platô em ~2MB |
| `cargo build` projeto grande | pico de dezenas de MB | platô em ~2MB |
| 10 abas com output contínuo | N × ∞ | N × ~2MB |

O `total_lines` é mantido incrementalmente — `+= block.line_count()` no commit, `-= old.line_count()` no despejo. Zero scan O(N) no hot path.

---

## 3. Problema II: Mapeamento Espacial (Hit-Testing)

### Causa raiz

O `glyphon` envia texto para o pipeline da GPU mas não expõe nenhuma API de hit-testing reverso. O estado da aplicação não tinha consciência de onde cada bloco estava na tela — impossível implementar seleção de texto, hover, ou colapso de blocos sem isso.

### Solução: `LayoutIndex` com busca binária

Dois novos tipos em `session.rs`:

```rust
pub struct BlockRect {
    pub y0: f32,   // topo do bloco em pixels de tela
    pub y1: f32,   // base do bloco em pixels de tela
    pub x0: f32,   // início do texto (após padding)
}

pub struct LayoutIndex {
    pub rects: Vec<BlockRect>,  // um por bloco, ordenado por y0 crescente
}
```

O `LayoutIndex` vive dentro do `BufferCache` e é reconstruído junto com os `GlyphonBuffer` — sempre sincronizado, zero estado desincronizado possível.

### Construção do índice

Durante `rebuild_buffers()`, antes de avançar `y`, cada bloco registra seu rect:

```rust
self.cache.layout.rects.push(BlockRect {
    y0: y,
    y1: y + content_h,
    x0: MARGIN_X + BLOCK_PAD_X,
});
```

Custo: uma alocação de `BlockRect` (24 bytes) por bloco visível. Negligenciável.

### API de hit-testing

```rust
pub fn hit_test(&self, x: f32, y: f32, font_w: f32) -> Option<HitResult>
```

Retorna:

```rust
pub struct HitResult {
    pub block_idx: usize,  // índice no snapshot atual
    pub line: usize,       // linha dentro do bloco (0 = comando, 1+ = output)
    pub col: usize,        // coluna aproximada
}
```

**Algoritmo:**

```
1. partition_point(|r| r.y0 <= y)  →  O(log k), k = blocos visíveis
2. Verifica se y ∈ [y0, y1) do candidato  →  O(1)
3. line = (y - y0 - BLOCK_PAD_Y) / FONT_H  →  O(1)
4. col  = (x - x0) / font_w               →  O(1)
```

`partition_point` é a busca binária da stdlib — retorna o primeiro índice onde a condição é falsa. Como os rects são construídos em ordem crescente de `y0`, a invariante de ordenação é garantida estruturalmente (não precisa de sort explícito).

**Por que não interval tree ou segment tree?**

Com k < 50 blocos visíveis numa tela típica, O(log 50) ≈ 6 comparações. Uma interval tree adicionaria complexidade de implementação e overhead de alocação sem nenhum ganho mensurável. `partition_point` sobre um `Vec` contíguo tem excelente localidade de cache — provavelmente mais rápido na prática do que qualquer estrutura de árvore para k pequeno.

**Por que coluna aproximada e não exata?**

Coluna exata exigiria shaping reverso — consultar o `GlyphonBuffer` para mapear X de volta ao índice de glifo. Isso é O(n_glifos_na_linha) e envolve acesso ao estado interno do cosmic-text. Para hover e highlight de bloco (Fase 4), a coluna aproximada por `x / font_w` é suficiente. Quando seleção de texto for implementada, o `Buffer::hit` do glyphon pode ser chamado pontualmente no bloco já identificado pelo `LayoutIndex`.

### Integração no event loop

O `CursorMoved` do winit agora chama o hit-test passivamente:

```rust
WindowEvent::CursorMoved { position, .. } => {
    let session = state.manager.current();
    if let Some(hit) = session.cache.layout.hit_test(
        position.x as f32, position.y as f32, FONT_W,
    ) {
        log::debug!("hit: block={} line={} col={}", hit.block_idx, hit.line, hit.col);
    }
}
```

O resultado é apenas logado por ora — a infraestrutura está pronta para ser consumida por hover, seleção e colapso de blocos na Fase 4.

---

## 4. Impacto no Loop de Renderização

Nenhuma das mudanças introduz trabalho extra no hot path de renderização:

- O despejo de blocos ocorre em `commit()` — chamado apenas quando o usuário pressiona Enter, nunca durante um frame.
- O `LayoutIndex` é reconstruído apenas quando `blocks.version()` muda — mesma condição do `rebuild_buffers()` existente.
- O hit-test em `CursorMoved` é O(log k) com k ≤ 50 — custo negligenciável, não bloqueia o frame.

A invalidação por versão da Fase 3 permanece intacta e continua sendo o mecanismo central de controle de reconstrução de buffers.

---

## 5. Estrutura de Arquivos

```
singularity/
├── Cargo.toml
└── src/
    ├── main.rs           — CursorMoved handler com hit-test passivo
    ├── session.rs        — LayoutIndex, BlockRect, HitResult, BufferCache
    ├── block_store.rs    — VecDeque, MAX_LINES, evict_if_needed (ALTERADO)
    ├── pty.rs            — inalterado
    └── terminal_state.rs — inalterado
```

---

## 6. Próximos Passos (Fase 4 — Visual)

Com memória limitada e hit-testing funcional, a Fase 4 pode implementar:

- **Backgrounds por bloco** — rect colorido (`#2C2C2E`) renderizado via wgpu antes do texto, usando `BlockRect.y0/y1` do `LayoutIndex` diretamente.
- **Hover highlight** — `HitResult.block_idx` identifica o bloco sob o cursor para mudar sua cor de fundo.
- **Seleção de texto** — `LayoutIndex` identifica o bloco, `Buffer::hit` do glyphon resolve o glifo exato dentro do bloco já identificado.
- **Colapso de blocos** — estado `collapsed: bool` por bloco; `rebuild_buffers` pula linhas de output se colapsado; `LayoutIndex` reflete a altura reduzida automaticamente.

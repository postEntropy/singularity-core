# Gargalos Arquiteturais em Emuladores de Terminal Baseados em Blocos: Complexidade de Memória e Mapeamento Espacial

**Projeto:** Singularity  
**Data:** Março de 2026  

---

## Resumo
O projeto *Singularity* implementou com sucesso uma arquitetura multi-sessão e não-bloqueante utilizando `winit`, `wgpu` e `glyphon`. No entanto, antes de implementar a interface de usuário visual estilo Warp (Fase 4), restam duas vulnerabilidades estruturais críticas. Este documento descreve o crescimento ilimitado de memória inerente à atual implementação do `BlockStore` e a falta de um sistema de mapeamento espacial bidirecional necessário para a interatividade. O objetivo da próxima fase de desenvolvimento é resolver esses gargalos.

## 1. Introdução
O motor atual processa a saída padrão (stdout) do pseudo-terminal (PTY) em blocos isolados por meio de uma máquina de estados finitos (`vte`). Embora essa arquitetura garanta alternância de contexto em $\mathcal{O}(1)$ entre sessões, ela introduz desafios específicos em relação ao ciclo de vida da memória e ao teste de colisão (*hit-testing*) que não existem em emuladores tradicionais baseados em grade (como o Alacritty).

## 2. Problema I: Complexidade de Espaço Ilimitada (A Bomba-Relógio da Memória)
Atualmente, o `BlockStore` anexa cada comando executado e sua respectiva saída (`StyledSpan`) em um vetor contínuo (`Vec<Block>`). Isso cria uma Complexidade de Espaço ilimitada de $\mathcal{O}(N)$, onde $N$ é o número total de linhas geradas durante o tempo de vida da sessão.

Em cenários do mundo real (por exemplo, leitura contínua de logs de servidor ou compilação de binários grandes em Rust), temos $N \to \infty$. Dadas as restrições de hardware padrão (como um teto de 6GB de VRAM em GPUs intermediárias), o `BufferCache` inevitavelmente esgotará a memória disponível ao tentar reter dados de glifos para todo o histórico.

### Implementação Requerida:
* **Estrutura de Dados Limitada:** Transicionar o armazenamento histórico de um vetor estritamente ilimitado para um Buffer Circular (*Ring Buffer*) ou uma lista virtualizada com uma capacidade máxima de linhas (ex: 10.000 linhas).
* **Estratégia de Despejo (*Eviction*):** Implementar um mecanismo de despejo altamente eficiente que descarte os blocos (ou blocos parciais) mais antigos quando o limite de capacidade for atingido, garantindo que o uso de memória atinja um platô em vez de crescer linearmente.

## 3. Problema II: Mapeamento Espacial Bidirecional (*Hit-Testing*)
O segundo déficit arquitetural é a desconexão entre o estado lógico e os pixels renderizados. O `glyphon` envia texto para o pipeline da GPU, mas o estado da aplicação não tem consciência espacial.

Para suportar recursos futuros, como seleção de texto, copiar/colar e retração de blocos, o motor requer uma função de mapeamento que traduza coordenadas 2D da tela de volta para o estado lógico da aplicação:

$$f: \mathbb{R}^2 \to \mathbb{N}^3$$

Onde uma coordenada de tela $(x, y)$ mapeia para uma tupla específica $(B_{id}, L, C)$ representando o ID do Bloco, a Linha e a Coluna.

### Implementação Requerida:
* **Indexação Espacial:** Criar um mecanismo (provavelmente dentro do `BufferCache` ou em um novo `LayoutManager`) que rastreie a caixa delimitadora (*bounding box*) de coordenadas $(X_0, Y_0, X_1, Y_1)$ de cada bloco renderizado e seus respectivos trechos de texto internos.
* **API de Hit-Testing:** Expor uma função que aceite coordenadas de mouse do `winit` e retorne o índice exato do caractere sob o cursor em tempo $\mathcal{O}(\log k)$ ou $\mathcal{O}(1)$, onde $k$ é o número de blocos visíveis.

## 4. Diretrizes para a Engenharia
Como Engenheiro de Sistemas Sênior, sua tarefa é refatorar as estruturas de dados centrais para resolver o **Problema I** e o **Problema II**.
Você mantém total liberdade técnica em relação aos algoritmos subjacentes e às *crates* Rust utilizadas. Foco estritamente na performance, minimizando alocações de *heap* durante o processo de despejo de memória e garantindo que a lógica de *hit-testing* não introduza latência no loop de eventos principal.

Por favor, forneça as implementações atualizadas para os módulos afetados (ex: `block_store.rs`, `session.rs`, `main.rs`), juntamente com uma breve justificativa técnica de suas escolhas algorítmicas.
# _legacy/ — 已知 broken 历史 demo

> **状态**: 这些 demo 引用了 v0.30 **未实现**的 runtime 特性. 它们在 v0.30 之前也已 broken
> (不只是 v0.30 引入的回归). 保留在此供历史参考, 不参与 CI 检查.

## 分类

### Parse error: Expected path (route / serve 块)
- `bench_server.mora` (v0.06)
- `http_server_demo.mora` (v0.06)
- `observe_route_server_demo.mora` (v0.10)
- `route_demo.mora` (v0.04)

### Type error: unknown type 'dyn:TraitName' (trait dispatch)
- `container.mora` (v0.08.5)
- `generic_with_where.mora` (v0.09)
- `nested_generic.mora` (v0.09)
- `trait_default_demo.mora` (v0.08.5)
- `trait_demo.mora` (v0.08.5)
- `trait_inherit_demo.mora` (v0.08.5)

### Parse error: 内部语法
- `document_parse_demo.mora` (v0.27) — list literal 含 dict
- `prompt_section_demo.mora` (v0.26) — prompt section 内部
- `skill_demo.mora` (v0.16) — 内部语法

### Lexer / Runtime 缺失
- `eval_demo.mora` (v0.25) — `!` 前缀 + `eval` builtin
- `orchestrate_demo.mora` (v0.06) — `@start` `@exit` graph 节点
- `memory_demo.mora` (v0.06) — `ai.chat` method chain
- `observe_demo.mora` (v0.10) — Observe v2 statement
- `office_ocr_demo.mora` (v0.28) — 需先 `mora-install-ocr`

## 修复策略

修复这些 demo 需要**先实现对应的 runtime 特性** (lexer/parser/AST/interpreter 全栈).
这是大工程, 不在 v0.30 范围内. 后续每个 demo 需要单独跟踪.

## CI 状态

`examples/_legacy/` 下文件不参与 `cargo run examples/*.mora` CI 检查.
`examples/` 根目录的 4 个 demo (`compact_demo`, `compress_demo`, `compress_smart_demo`,
`mcp_server_demo`) 必须能跑通.

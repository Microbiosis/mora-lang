---
kind: configuration_system
name: Mora  + CLI 
category: configuration_system
scope:
    - '**'
source_files:
    - src/main.rs
    - src/lib.rs
---

## 1. 
- **** `config``serde_yaml``dotenv`  `std::env`  `std::env::args()` 
- ****AI  `AI_API_KEY_ENV``AI_BASE_URL_ENV``AI_BASE_URL_DEFAULT` `main.rs`  `print_banner` 
- **** `.moraignore``mora.lock``.mora/recordings/*.jsonl``.mora/snapshots/*.snap.jsonl` CLI 

## 2. 
- `src/main.rs` CLI run / record / replay / diff / snapshot / mcp / install … AI 
- `src/lib.rs` `VERSION`  main 
- `src/interpreter/mod.rs` `AI_API_KEY_ENV``AI_BASE_URL_ENV``AI_BASE_URL_DEFAULT`  `main.rs`  `print_banner` 
- `src/mcp_server.rs` `builtin_toolsets()` `mora mcp tool-list/tool-search/toolsets` 
- `src/record/mod.rs`/// JSONL CLI  `Recorder::new_record/new_replay` 
- `Cargo.toml` `mora`  `mora-lsp` 

## 3. 
- ****CLI  →  → 
- **AI ** `OPENAI_API_KEY` `OPENAI_BASE_URL` mock  `route` + `with` 
- ****`mora.lock`  `<cwd>/.mora/recordings/<name>.jsonl` `<cwd>/.mora/snapshots/<name>.snap.jsonl` `<cwd>/.moraignore` `--policy <file>` 
- ****`mora install <url>`  `<cwd>/vendor/<pkg>.mora` `mora.lock` `name = "url"`
- ****VS Code / Neovim / Helix / Sublime / Vim / Emacs  `editors/`  LSP  Mora 

## 4. 
1. **** `src/interpreter/mod.rs`  `XXX_ENV``XXX_DEFAULT` `main.rs`  `env::var(XXX_ENV).unwrap_or(XXX_DEFAULT)` 
2. ** TOML/YAML **“ + CLI ”
3. **CLI ** `main.rs`  `--format``--output``--policy``--update` 
4. ** `.mora/` ** CI 
5. ****API KeyBase URL  banner  real/mock 
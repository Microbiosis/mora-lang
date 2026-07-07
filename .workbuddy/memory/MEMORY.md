# Mora-lang 

## 
AI Agent  DSL + Rust 
 v0.55 baselineCargo 0.0.55863 tests pass94 36,874 LOCsrc

## 
- **C1**  async runtime —— tokio http/mcp/lsp
- **C2** / —— 17  Rust crate serde JSON
- **C3**  v1.0  —— /HM/

## 9  Bounded Context
BC1 Language core (shared kernel) / BC2 Execution runtime (hub) / BC3 AI / BC4 Orchestration / BC5 Persistence / BC6 Transport / BC7 Sandbox / BC8 Document IO / BC9 Scheduling

##  docs/ARCHITECTURE_DESIGN_v2.md
- Interpreter god object  7 runtime facadeCore/Registry/Infra/AI/Sandbox/Persist/Orch 35  #1  facade  pub  + builtins.rs 5100 
- sync-first async spawn_blocking 
-  VM  Plateau C
-  + property test v1.0 

##  docs/METAMORPHOSIS_ROADMAP.md
- **** Rust/Go"AI "——mora 
- ****α IR Plateau C/ β Plateau B/ γ AI B mora 
- **γ ** AI  AI typeck record/replay 
- **5 **I1 AI-native  / I2  / I3  async / I4  v1.0 / I5 
- ****β  γ AI  typeck 

## 4 Plateau
- A (v0.52-v0.55) facade  / ~~ASTv2 ~~ / ~~ CheckpointSaver~~ /  JSON+Value::Command-Send /  builtins/dispatch/main / unwrap 473 
- B (v0.56-v0.65)  + property test + HM  + bench 
- C (v0.66-v0.75) VM/ async  / GPU 
- D (v0.76+)  / FFI / 

## 2026-07-11 
- Interpreter  7 7  pub(crate) facade holder 35  runtime/* facade 
- unwrap 473 ↑50 builtins.rs 85 checkpoint/sqlite.rs 104/
-  974473 unwrap + 173 panic + 328 expect
- builtins.rs 5098 LOC / dispatch.rs 1417 / main.rs 1043 
- ai_infra.rs 783  65 dead_code 8.3% 
- ai_chat.rs 865  Bug 
- runtime ↔ interpreter  5 
- runtime 34  pub 
- TokenType ~120 / Type 30 / Value 33 / ExprKind 22 / StmtKind 48
- p"..."  AI AI  Conversation.chat / Agent.run
-  ADR Context  grep pub 

## AGENTS.md
-  MCP 
-  unwrap()/panic! ?  expect("")
- cargo build/test/clippy -D warnings/fmt --check 
- v0.x  breaking

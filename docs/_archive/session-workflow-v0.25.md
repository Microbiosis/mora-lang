# Mora v0.25 

> ****: 2026-06-30
> ****: Microbiosis + ZCode AI
> ****: V2  +  + Bug  + 
> ****: ——

---

## 

1. [](#1-)
2. [Phase 1: V2 AST ](#2-phase-1-v2-ast-)
3. [Phase 2: Bug ](#3-phase-2-bug-)
4. [Phase 3: ](#4-phase-3-)
5. [Phase 4: ](#5-phase-4-)
6. [](#6-)
7. [](#7-)
8. [](#8-)

---

## 1. 

### 1.1 

Mora  AI  Rust  v0.24 ****

```
Source → ParserV2 → v2 AST → AstV2ToV1() → v1 AST → typeck(v1) → interpreter(v1)
```

ParserV2  v2 AST typeck  interpreter  v1 `AstV2ToV1` 

1. ****:  v2→v1 
2. ****:  AST 
3. ****: 

### 1.2 

```
Phase 1: V2  (8 steps)
   Step 1:  common.rs 
   Step 2: 
   Step 3:  v2
   Step 4:  execute_v2/evaluate_v2
   Step 5:  typeck check_stmt_v2
   Step 6: 
   Step 7:  v1  + 
   Step 8: 

Phase 2: Bug  (36 )
   Match 
   Partial Trait dispatch
   Arena  body
   typeck 

Phase 3:  (5 )
   Multi-Agent orchestrate (sequential/graph/loop)
   Eval  ( + tolerance)
   Skill  ()
   Memory  (store/recall/search/save/load)
   Context Compaction (compact + Conversation.compact)

Phase 4:  (7 )
   ai_infra.rs (600 )
   interpreter/ai_chat.rs (826 )
   interpreter/ai_helpers.rs (368 )
   interpreter/builtins.rs (290 )
   interpreter/dispatch.rs (1047 )
   interpreter/orchestrate.rs (222 )
   interpreter/trait_dispatch.rs (168 )
```

### 1.3 

```
: 202/202  
clippy: 
:  363  3396  3033 
```

---

## 2. Phase 1: V2 AST 

### 2.1 Step 1:  common.rs

****: v1  v2 AST Span, Literal, BinaryOp 

****:
1.  `src/common.rs` 6 
2.  `ast_v2.rs` `use crate::common::*`
3.  `parser_v2.rs` 
4.  `ast.rs`  re-export + v1 

****:
```rust
// common.rs
pub struct Span { pub line: usize, pub column: usize }
pub enum Literal { String(String, Span), Char(char, Span), Number(f64, Span), Bool(bool, Span), Nil(Span) }
pub enum BinaryOp { Add, Sub, Mul, Div, Mod, Equal, NotEqual, Greater, Less, GreaterEqual, LessEqual }
pub struct GenericParam { pub name: String, pub bound: Option<String>, pub span: Span }
pub struct EnumVariant { pub name: String, pub data: Option<String> }
pub struct StructField { pub name: String, pub type_hint: String }
```

****: 

### 2.2 Step 2: 

****:
-  `typed_ast.rs` (v1→v2 )
-  `ast_adapter.rs` ( v1→v2 )

****:  `grep -rn` 

### 2.3 Step 3: 

****—— v2→v1 

****:
1.  `ast_v2_to_v1.rs`
2.  `main.rs` 
```rust
// : ParserV2 → AstV2ToV1 → v1 AST → typeck(v1) → interpret(v1)
// : ParserV2 → v2 AST → typeck_v2(v2) → interpret_v2(v2)
fn run_file(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");
    let (node_ids, arena) = parse_with_v2(&source);
    let type_errors = typeck::check_program_v2(&node_ids, &arena);
    // ... error handling ...
    let mut interpreter = Interpreter::new();
    interpreter.interpret_v2(&node_ids, &arena)?;
}
```

****: typeck_v2 

### 2.4 Step 4-5:  v2 

****:
- `execute_v2`:  23/39  39/39 StmtKind 
- `check_stmt_v2`:  6/39  39/39 
- `check_expr_v2`:  6/21  14/21 

**** ( v1  v2):
```rust
// v1: Box<Expr> / Vec<Stmt>
fn check_expr(&mut self, expr: &Expr, symbols: &SymbolTable) -> Type { ... }

// v2: NodeId + arena 
fn check_expr_v2(&mut self, expr_id: NodeId, arena: &AstArena, symbols: &SymbolTable) -> Type {
    let expr = arena.get_expr(expr_id).unwrap();
    match &expr.kind {
        ExprKind::Literal(lit) => { ... }
        ExprKind::Binary { left, op, right } => {
            let left_ty = self.check_expr_v2(*left, arena, symbols);
            // ...
        }
    }
}
```

****: v1→v2  `Box<Expr>` → `NodeId` + arena `arena.get_expr(id)`

### 2.5 Step 6-7:  v1 

****:
-  `ast_v2_to_v1.rs` 
-  v1 `interpret()`, `execute()`, `evaluate()`  (~1700 )
-  `interpret_v2` → `interpret`, `execute_v2` → `execute`

****:  `match_pattern`  `evaluate_v1_expr` guard  v1 `Expr` 

****:  `grep -rn "function_name"` 

---

## 3. Phase 2: Bug 

### 3.1  (Parser)

****: `classify(-3)`  `classify(- 3)`

****:  parser  `unary()` 
```rust
fn unary(&mut self) -> NodeId {
    if self.check(&TokenType::Minus) {
        let span = self.span_of_current();
        self.advance();
        let operand = self.unary();
        let zero = self.arena.alloc_expr(
            ExprKind::Literal(Literal::Number(0.0, span)), span
        );
        let kind = ExprKind::Binary { left: zero, op: BinaryOp::Sub, right: operand };
        self.arena.alloc_expr(kind, span)
    } else { ... }
}
```

****:  `-x`  `0 - x` AST 

### 3.2 Arena 

****: `memory.store("key", "val")` 

****: `self.environment`  `self.globals`  `Arc<Mutex<Environment>>``evaluate_v2`  Call  `.or_else(|| globals.lock())` 

```rust
// 
let func_val = self.environment.lock().expect("env").get(callee)
    .or_else(|| self.globals.lock().expect("globals").get(callee));  // 

//  clone 
let func_val = self.environment.lock().expect("env").get(callee);  //  clone
```

****:  Arc  Mutex  lock  clone 

### 3.3 Trait  dispatch

****: `test_trait_default_implementation_fallback` —— `self.value()` 

****: v2 TaskDef handler  `v2_body_ids: vec![]` body 

****: 
```rust
// : body 
v2_body_ids: vec![],

// :  body
let body_ids: Vec<usize> = body.iter().map(|id| id.0).collect();
v2_body_ids: body_ids,
```

****: v2  Task  Closure  v1 `Vec<Stmt>` body `v2_body_ids` / `v2_node_id`  arena ID

### 3.4 Dict 

****: `Greeter.greet("World")`  "Dict has no method: greet"

****: Skill  task  Dict  Dict  Dict 

****:  Dict  catch-all  Dict  callable 
```rust
// 
_ => Err(format!("Dict has no method: {}", method)),

// 
_ => {
    if let Some(val) = map.get(method) {
        match val {
            Value::Task { .. } | Value::Closure { .. } => {
                return self.call_value(val, args);
            }
            _ => {
                //  callable  metadata 
                if args.is_empty() { return Ok(val.clone()); }
            }
        }
    }
    Err(format!("Dict has no method: {}", method))
}
```

****: Dict 

---

## 4. Phase 3: 

### 4.1 Multi-Agent orchestrate

****: `docs/multi-agent-design.md`

****:
```mora
-- Sequential: 
orchestrate sequential input -> result
  agent a task(ai.chat(p"Step 1: {input}")) end
  agent b task(ai.chat(p"Step 2: {input}")) end
end

-- Graph:  + 
orchestrate graph input -> result
  agent a task(...) end
  agent b task(...) end
  edges
    @start -> a
    a -> b when rounds < 2
    b -> @exit
  end
end

-- Loop: 
orchestrate loop input -> result, max_rounds: 3
  agent improver task(ai.chat(p"Improve: {input}")) end
  exit_when: result.contains("done")
end
```

**AST **:
```rust
pub enum OrchestrateKind {
    Sequential { agents: Vec<OrchestrateAgent> },
    Graph { agents: Vec<OrchestrateAgent>, edges: Vec<OrchestrateEdge> },
    Loop { agent: OrchestrateAgent, max_rounds: usize, exit_when: Option<NodeId> },
}

pub struct OrchestrateAgent {
    pub name: String,
    pub with_config: Option<Vec<(String, NodeId)>>,
    pub task_expr: NodeId,
    pub verify_expr: Option<NodeId>,
}

pub struct OrchestrateEdge {
    pub from: String,
    pub to: String,
    pub condition: Option<NodeId>,
}
```

**** (Graph ):
```rust
fn execute_orchestrate(&mut self, ...) -> Result<FlowSignal, String> {
    match kind {
        OrchestrateKind::Graph { agents, edges } => {
            let mut current = input;
            let mut current_node = "@start".to_string();
            let mut rounds_map: HashMap<(String, String), usize> = HashMap::new();

            loop {
                // 
                let next_edge = edges.iter().find(|e| {
                    e.from == current_node && match &e.condition {
                        Some(cond_id) => self.evaluate(*cond_id, arena)
                            .map(|v| matches!(v, Value::Bool(true)))
                            .unwrap_or(false),
                        None => true,
                    }
                });

                match next_edge {
                    None | Some(OrchestrateEdge { to: "@exit", .. }) => break,
                    Some(edge) => {
                        let agent = agents.iter().find(|a| a.name == edge.to).unwrap();
                        *rounds_map.entry((edge.from.clone(), edge.to.clone())).or_insert(0) += 1;
                        current = self.run_orchestrate_agent(agent, &current, arena)?;
                        current_node = edge.to.clone();
                    }
                }
            }
        }
    }
}
```

### 4.2 Eval 

****:
```mora
eval ""
  given: sample_code
  expect: result.contains("error")
  expect: result.len() > 50
  tolerance: 0.8
  replay: "recordings/test.jsonl"
end
```

****:
- `given` 
- `tolerance`  LLM 
- `replay`  record/replay 

### 4.3 Skill 

****:
```mora
skill CodeReviewer
  description: ""
  version: "1.0.0"
  requires: [git, diff]

  task review(code: string): string
    return ai.chat(p"\n{code}")
  end

  task summarize(review: string): string
    return ai.chat(p"\n{review}")
  end

  verify(result: string)
    return result.len() > 0
  end
end
```

****: Skill  Dicttask  Dict 
```mora
-- :
let CodeReviewer = {
  "name": "CodeReviewer",
  "description": "",
  "version": "1.0.0",
  "requires": ["git", "diff"],
  "review": Task { name: "review", params: ["code"], v2_body_ids: [...] },
  "summarize": Task { ... },
  "verify": Task { ... }
}
```

****: `CodeReviewer.review("code")`  Dict  Dict  `review` 

### 4.4 Memory 

**API**:
```mora
memory.store("key", value)    -- 
memory.recall("key")          -- 
memory.search("query")        -- key 
memory.forget("key")          -- 
memory.clear()                -- 
memory.save("./data.json")    -- JSON 
memory.load("./data.json")    -- 
memory.size()                 -- 
memory.keys()                 -- 
```

****: `HashMap<String, Value>` + JSON 

### 4.5 Context Compaction

**API**:
```mora
-- 
let summary = compact(long_text)

-- 
let conv = ai.new_conversation("gpt-4")
conv.chat("...")
let summary = conv.compact()

-- 
with model("gpt-4"), compact_at(80) do
  -- token  80% 
end
```

****: builtin ai/web/json/file/memory/agent `Interpreter::new()` 
```rust
for name in &["ai", "web", "json", "file", "memory", "agent"] {
    globals.lock().unwrap()
        .define(name.to_string(), Value::Builtin(name.to_string()), false);
}
```

---

## 5. Phase 4: 

### 5.1 

 `interpreter.rs`7286 

```
: interpreter.rs (7286 )
: interpreter/
 mod.rs            (3402  — )
 ai_chat.rs        (826 )
 ai_helpers.rs     (368 )
 builtins.rs       (290 )
 dispatch.rs       (1047 )
 orchestrate.rs    (222 )
 trait_dispatch.rs (168 )
```

### 5.2 Rust 

```rust
// src/interpreter/mod.rs
mod ai_chat;
mod ai_helpers;
mod builtins;
mod dispatch;
mod orchestrate;
mod trait_dispatch;

use crate::ai_infra::*;  // 
use crate::flow::*;
// ...  Interpreter  ...
```

```rust
// src/interpreter/dispatch.rs
use super::*;  // 
use crate::common::Span;
use crate::value::Value;

impl Interpreter {
    pub(super) fn call_function(...) -> Result<Value, String> { ... }
    pub(super) fn call_method(...) -> Result<Value, String> { ... }
    pub(crate) fn call_value(...) -> Result<Value, String> { ... }
}
```

****:
- Rust  `impl` 
- `pub(super)` 
- `pub(crate)`  crate 
-  `use super::*` 

### 5.3 

****: `call_value`  `ai_chat.rs`  `builtins.rs`  `pub(super)` 

****:  `pub(crate)` `pub(super)`

### 5.4 

1. `ai_infra.rs`— 
2. `ai_helpers.rs`— 
3. `dispatch.rs`, `builtins.rs`—  `impl` 
4. `ai_chat.rs`, `trait_dispatch.rs`— 

---

## 6. 

|  |  |
|------|------|
|  |  |
|  common.rs |  v1/v2  |
|  match_pattern  v1  | guard  v1 Expr |
| orchestrate  `orchestrate`  |  parallel/transaction  |
| Skill  Dict |  Dict  |
| Memory  HashMap | JSON  |
| builtin  | environment.get()  |
| `pub(super)` vs `pub(crate)` |  `pub(crate)` `pub(super)` |

---

## 7. 

### 7.1 

****: `self.environment`  `self.globals`  `Arc<Mutex<Environment>>`

```rust
//  
let val = self.environment.lock().get(key)
    .or_else(|| self.globals.lock().get(key));  //  Mutex!

//   clone 
let val = self.environment.lock().get(key).cloned();
```

### 7.2 sed 

****:  `sed -i '100,200d'`  impl 

****:  `grep -n "^    }$"` 

### 7.3 

****: `given``description``version`  `Greeter.description` 

****: 

### 7.4 Arena  vs 

****: `call_value`  arena  v2  arena  `interpret_v2`  `&AstArena` 

****:  Interpreter  `v2_arena: Option<AstArena>``interpret_v2`  clone `call_value` 

### 7.5 Builtin 

****: `memory.store("key", "val")`  "Undefined variable: memory"

****: `memory`  `is_builtin_object()`  `Interpreter::new()` 

****:  `new()`  builtin 

---

## 8. 

### 8.1 

|  |  |  |  |
|------|------|------|------|
| interpreter.rs | 7286 | 3402 | **-3884** |
| typeck.rs | ~4400 | 2838 | **-1562** |
| ast.rs | 439 | 0 | **-439** |
| ast_v2_to_v1.rs | 503 | 0 | **-503** |
| typed_ast.rs | 605 | 0 | **-605** |
| ast_adapter.rs | 588 | 0 | **-588** |
|  common.rs | 0 | 73 | +73 |
|  ai_infra.rs | 0 | 600 | +600 |
|  interpreter/*.rs | 0 | 2921 | +2921 |
| **** | | | **-3387** |

### 8.2 

|  |  |  |
|------|------|------|
|  | 188 | 0 |
| Step 1-6 | 150 | 38 |
| Step 7 | 148 | 40 |
| Bug  | 202 | 0 |

### 8.3 

```
1af52fd refactor:  interpreter — AI  → ai_helpers.rs
1afc336 refactor:  interpreter —  → dispatch.rs
5bb93b3 refactor:  interpreter — Trait  → trait_dispatch.rs
7413182 refactor:  interpreter — AI  → ai_chat.rs
4e05abd refactor:  interpreter —  → builtins.rs
aa24fb6 refactor:  interpreter.rs — AI  → ai_infra.rs
e5d10a7 refactor:  interpreter —  + orchestrate 
6c42ded feat: Memory + Context Compaction  + builtin 
ec0a671 feat: Eval replay  + 
b058095 feat:  Eval + Skill 
f2be108 feat:  Multi-Agent orchestrate 
ba90114 fix:  — 188/188 
143e53f fix:  typeck check_expr_v2
d716bf7 fix:  bug — for/stringguardtrait dispatchpartial
4e5ca15 fix:  match_pattern guard 
d919c1e refactor:  Value::Task/Closure/Macro  v1 body 
463d3a7 refactor:  v1 execute/evaluate + 
1704df5 refactor:  v1 interpret +  v2 task 
8c30780 refactor:  ast_v2_to_v1.rs
e70a9b6 refactor:  typeck check_stmt_v2
2b1e30b refactor:  execute_v2 + evaluate_v2
313ac12 refactor:  v2
915857a refactor:  + 
```

---

## : 

### A. 

1. **** — 
2. **** — 
3. **** — 
4. **** — 
5. ** commit** —  `git revert`

### B. Rust 

```rust
// : src/interpreter/mod.rs + src/interpreter/*.rs
mod ai_chat;       // 
mod builtins;

// 
use super::*;      //  pub 

// 
pub(super) fn helper() {}   // 
pub(crate) fn public() {}   //  crate 

//  impl
// src/interpreter/dispatch.rs
impl Interpreter {
    pub(super) fn call_function(...) { ... }
}
```

### C. 

- [ ] 
- [ ] 
- [ ] 
- [ ] 
- [ ] 188/188 
- [ ]  commit

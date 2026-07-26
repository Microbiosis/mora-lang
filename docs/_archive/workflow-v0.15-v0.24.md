# Mora v0.15 → v0.24 

>  v0.14 (record/replay/diff)  v0.24 (ParserV2 )

---

## 

```
v0.14 (record/replay/diff)
  ↓  TODO
v0.15 (AI config + record )
  ↓ 
v0.16 ( - Prolog)
  ↓ 
v0.17 ( +  - StreamIt/APL)
  ↓ 
v0.18 (compose/take/drop/partial - Clojure/Lisp)
  ↓ 
v0.19 (Worker +  + atom - Ballerina/Clojure)
  ↓ 
v0.20 ( +  - Smalltalk/Common Lisp)
  ↓ 
v0.21 ( +  - Rust)
  ↓ 
v0.22 (AI  +  +  - DeepSpec)
  ↓ 
v0.23 ( +  + )
  ↓ AST 
v0.24 (ParserV2  +  Parser )

## v0.24 ParserV2 

### :  

 parser.rs (2459 )  ParserV2

### 

```
 .mora → Lexer → Token  → ParserV2 → ASTv2 →  → AST → 
```

### 

|  |  |  |
|------|------|------|
| src/parser_v2.rs | 1766 | ParserV2  |
| src/ast_v2.rs | 543 | AST v2  |
| src/ast_v2_to_v1.rs | 388 |  |
| src/interpreter.rs | - |  ( parse_code) |
| src/typeck.rs | - |  ( parse_code) |

### 

1. **ParserV2 ** (1766 )
   -  parser.rs (2459 )
   - 
   -  ast_v2 

2. **** (ast_v2_to_v1.rs)
   -  ast_v2  ast
   -  AST

3. ****
   - let : 
   - string + any: 

4. ****
   - interpreter.rs:  parse_code()
   - lsp:  ParserV2
   - typeck :  parse_code()
   -  parser.rs

5. **Bug **
   - : 
   - trait/impl :  break guard
   - transaction rollback/commit: 
   - list/dict : 
   - match when : 

### 

- : 188 passed
- : 5 passed ( .mora )
- CI:  (Check + Rustfmt + Clippy + Test + LSP Smoke + Record CLI)

### 

- container.mora 
- nested_generic.mora 
- observe_demo.mora 
- trait_demo.mora 
- trait_default_demo.mora 

### 

|  |  |
|------|------|
|  | let, task, return, if, for, import, break, continue, match, with, parallel, worker, transaction, macro, route, trait, impl, type, enum, struct, save, load, read, write, append, read_bytes, write_bytes, stream, tool, observe, span, record_tokens, commit, rollback |
|  | variable, literal, binary, unary, call, method_call, index, question, closure, pipe, list, dict, match, prompt, format_string, ai_model, namespace_ref, char |
|  | literal, variable, wildcard, list, dict, guard |
|  | generic_params, type_list, type_name_recursive, where_clause, dyn trait |
   - lsp:  ParserV2
   - typeck :  parse_code()
   -  parser.rs

### 

- : 186 passed
- : 5 passed ( .mora )
- clippy: clean

### 

- container.mora 
- nested_generic.mora 
- observe_demo.mora 
- trait_demo.mora 
- trait_default_demo.mora 
```

---

##  1:  (v0.15)

### 1.1 
```bash
cargo build          # 
cargo test           # 
cargo clippy         # 
cargo fmt -- --check # 
```

### 1.2 
```bash
rustup component add rustfmt clippy
```

### 1.3  TODO
```bash
grep -rn "TODO\|FIXME" src/ --include="*.rs"
```

### 1.4  TODO
```rust
// : TokenBudget.per_call
// 1. 
// 2. 
// 3.  TODO 
// 4. 
```

---

##  2:  (v0.16-v0.20)

### 2.1 
```bash
# 
cat docs/learning-plan.md
```

### 2.2  AST
```rust
// src/ast.rs
pub enum Stmt {
    // 
    NewFeature { ... },
}

pub enum Expr {
    // 
}
```

### 2.3  Lexer
```rust
// src/lexer.rs
pub enum TokenType {
    // 
    NewKeyword,
}

//  identifier_from 
"new_keyword" => TokenType::NewKeyword,
```

### 2.4  Parser
```rust
// src/parser.rs
fn new_feature_statement(&mut self) -> Stmt {
    // 
}
```

### 2.5  Interpreter
```rust
// src/interpreter.rs
//  execute 
Stmt::NewFeature { .. } => {
    // 
}

//  call_method 
"new_method" => {
    // 
}
```

### 2.6  TypeChecker
```rust
// src/typeck.rs
// 
```

### 2.7  LSP
```rust
// src/lsp/providers.rs
// 
```

### 2.8 
```rust
#[test]
fn test_new_feature() {
    let src = r#"
task main()
  // 
end
"#;
    run(src).expect("should work");
}
```

### 2.9 
```bash
cargo build && cargo test && cargo clippy
```

---

##  3:  (v0.22)

### 3.1 AI 
```rust
// AI 
let cache_key = format!("{}:{:?}", model, messages);
if let Some(cached) = self.ai_cache.get(&cache_key) {
    return Ok(Value::String(cached.clone()));
}

// 
with speculative = true, draft_model = "gpt-4o-mini"
  let result = ai.chat("question")
end

//  AI 
let results = batch_chat(["q1", "q2", "q3"])
```

### 3.2 
```rust
//  - 
fn is_fusable_method(method: &str) -> bool {
    matches!(method, "map" | "filter" | "take" | "drop")
}
```

### 3.3 
```rust
// 
fn try_fold_binary(left: &Value, op: &BinaryOp, right: &Value) -> Option<Value> {
    match (left, op, right) {
        (Value::Number(l), BinaryOp::Add, Value::Number(r)) => Some(Value::Number(l + r)),
        // ...
    }
}
```

### 3.4 
```rust
// 
fn intern_string(&mut self, s: String) -> Value {
    if let Some(interned) = self.string_interner.get(&s) {
        return interned.clone();
    }
    let val = Value::String(s.clone());
    self.string_interner.insert(s, val.clone());
    val
}
```

---

##  4:  (v0.23)

### 4.1 
```rust
// src/ast.rs
pub enum Stmt {
    TypeAlias {
        name: String,
 generics: Vec<String>,
        target: String,
        span: Span,
    },
}
```

### 4.2 
```rust
pub enum Stmt {
    EnumDef {
        name: String,
        generics: Vec<String>,
        variants: Vec<EnumVariant>,
        span: Span,
    },
}

pub struct EnumVariant {
    pub name: String,
    pub data: Option<String>,
}
```

### 4.3 
```rust
pub enum Stmt {
    StructDef {
        name: String,
        generics: Vec<String>,
        fields: 

### 5.1 
```bash
# docs/mora-spec.md
# 
```

### 5.2  CHANGELOG
```bash
# CHANGELOG.md
## [v0.XX] - YYYY-MM-DD
### 
- 
```

### 5.3 
```bash
# docs/learning-plan.md
# 
```

---

##  7: CI/CD 

### 7.1 
```yaml
# .github/workflows/ci.yml
jobs:
  check: cargo check --all-targets
  test:  # 
  fmt:   cargo fmt --all -- --check
  clippy: cargo clippy --all-targets --all-features -- -D warnings
```

### 7.2 
```toml
# Cargo.toml
version = "0.0.XX"
```

---

##  8: 

### 8.1 README.md
```markdown
# 
### v0.XX-v0.YY 

|  |  |  |  |
|------|------|------|------|
| v0.XX |  |  | `` |
```

### 8.2 AGENTS.md
```markdown
## 
- clippy: 0 warnings
- : N passed
```

### 8.3 CLAUDE.md
```markdown
## 
src/
 value.rs      # 
 flow.rs       # 
 interpreter.rs # 
```

### 8.4 
```bash
# Cargo.toml: 
# Dockerfile: 
# docker-compose.yml: 
# .gitignore: 
```

---

##  9: 

### 9.1 
```bash
cargo build          #  0 errors
cargo test           #  N passed
cargo clippy         #  0 warnings
cargo fmt -- --check #  formatted
```

### 9.2 
```bash
git add -A
git commit -m "feat(v0.XX): "
```

### 9.3 
```bash
git log --oneline origin/main..HEAD | wc -l
git diff --stat origin/main
```

---

## 

```bash
# 
cargo build && cargo test && cargo clippy

# 
cargo fmt

# 
cargo test test_name

# 
cargo test 2>&1 | grep "test result"

#  clippy 
cargo clippy 2>&1 | grep "warning:"

# 
git add -A && git commit -m "feat: description"

# 
git log --oneline -10

# 
wc -l src/*.rs
```

---

## 

- [ ] 
- [ ]  clippy 
- [ ] 
- [ ] CHANGELOG 
- [ ] 
- Mora  Mora 
    -  Mora  `let` `assign` 
- Mora  |  |> 
    - Mora  `|>`  `|`
- Mora  task 
    - Mora  task  `:`  `->`
- Mora  match 
    - Mora  match  `match expr with pattern -> result end`
- Mora 
    - Mora  `fn(x) x * 2 end`  `fn(x) return x * 2 end`

---

*v0.15 → v0.24  — 2026-06-29*

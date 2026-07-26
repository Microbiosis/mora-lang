# Mora  (v0.11 → v0.23)

>  v0.11  v0.20 

---

## 

```
v0.11 (HTTP )
  ↓ 
v0.12 (MCP stdio transport)
  ↓ 
v0.13 ( Type::Any + Walrus )
  ↓ 
v0.14 (record/replay/diff CLI)
  ↓ AI config 
v0.15 (AI config + record )
  ↓ 
v0.16 (match  + ...rest + Dict)
  ↓ 
v0.17 ( +  + )
  ↓ 
v0.18 (compose/take/drop/partial)
  ↓ 
v0.19 (Worker +  + atom)
  ↓ 
v0.20 ( +  + )
```

---

## v0.11: HTTP 

### 
-  HTTP server 
-  SO_REUSEADDR + 

### 
```bash
# 1. 
cargo build && cargo test

# 2. 
# src/http_server.rs: 

# 3. 
cargo test

# 4. 
git commit -m "v0.11: HTTP server "
```

### 
- HTTP server  4 
-  SO_REUSEADDR

---

## v0.12: MCP stdio transport

### 
-  MCP  stdio 

### 
```bash
# 1.  MCP stdio
# src/mcp_server.rs:  stdio transport

# 2. 
cargo test

# 3. 
git commit -m "v0.12: MCP stdio transport"
```

---

## v0.13: 

### 
-  Type::Any 
-  Walrus  (:=)

### 
```bash
# 1.  Type::Any
# src/typeck.rs:  Any 

# 2.  Walrus 
# src/parser.rs:  := 

# 3. 
cargo build 2>&1 | grep "error"

# 4. 
cargo test

# 5. 
git commit -m "feat(typeck): v0.13  -  Type::Any  +  Walrus  (breaking)"
```

---

## v0.14: record/replay/diff CLI

### 
-  AI //
-  FlightBox 

### 
```bash
# 1.  record.rs 
# src/record.rs:  Recorder, Event, load/save

# 2.  CLI 
# src/main.rs:  record/replay/diff 

# 3.  interpreter
# src/interpreter.rs:  ai.chat 

# 4. 
cargo test

# 5. 
git commit -m "feat(record): v0.14 record/replay/diff CLI"
```

### 
```bash
mora record script.mora demo-001   # 
mora replay script.mora demo-001   # 
mora diff demo-001 demo-002        # 
```

---

## v0.15: AI config + record 

### 
-  5  TODO
-  record CLI (list/stats/timeline/export/audit/report/snapshot/mock_llm)

### 
```bash
# 1.  TODO
grep -rn "TODO" src/ --include="*.rs"

# 2.  TokenBudget.per_call
# src/interpreter.rs: track_tokens 

# 3.  AiConfig (max_tokens/system/temperature)
# src/interpreter.rs: real_ai_chat_with_tools 

# 4.  mock_llm
# src/interpreter.rs: with mock_llm = [...]

# 5.  record CLI
# src/record.rs: list_recordings, compute_stats, build_timeline
# src/main.rs: 

# 6. 
cargo test

# 7. 
git commit -m "feat(v0.15): AI config  + record CLI "
```

### 
```bash
mora record list              # 
mora record stats <name>      # 
mora record timeline <name>   # 
mora record export <name>     #  JSONL/Markdown
mora record audit <name>      # 
mora record report <name>     # 
mora snapshot <file> <name>   # 
```

---

## v0.16:  (Prolog)

### 
- match 
-  ...rest 
- Dict 

### 
```bash
# 1.  AST
# src/ast.rs: Pattern  Guard, List{prefix,rest}

# 2.  Lexer
# src/lexer.rs:  ... (DotDotDot) token

# 3.  Parser
# src/parser.rs:  when , ...rest 

# 4.  Interpreter
# src/interpreter.rs: match_pattern  Guard  rest

# 5.  TypeChecker
# src/typeck.rs: 

# 6. 
cargo test guard

# 7. 
git commit -m "feat(v0.16):  (Prolog)"
```

### 
```mora
-- 
match n with
  x when x > 0 -> "positive"
  _ -> "zero"
end

--  rest
let [head, ...tail] = [1, 2, 3]

-- Dict 
match data with
  {name: n} -> n
end
```

---

## v0.17:  (StreamIt/APL)

### 
- 
- window/batch 
- shape/flatten/transpose/reshape 
- 

### 
```bash
# 1. 
# src/interpreter.rs: evaluate_pipe 

# 2. List 
# src/interpreter.rs:  window/batch/shape/flatten/transpose/reshape

# 3. 
# src/interpreter.rs: numeric_op  list

# 4. 
cargo test

# 5. 
git commit -m "feat(v0.17):  (StreamIt/APL)"
```

### 
```mora
-- 
5 |> fn(x) return x * 2 end

-- 
[1,2,3,4,5].window(3)   -- [[1,2,3],[2,3,4],[3,4,5]]
[1,2,3,4,5].batch(2)    -- [[1,2],[3,4],[5]]

-- 
[[1,2],[3,4]].shape()      -- [2, 2]
[[1,2],[3,4]].flatten()    -- [1, 2, 3, 4]
[[1,2],[3,4]].transpose()  -- [[1,3],[2,4]]

-- 
[1, 2, 3] * 2    -- [2, 4, 6]
1 + [10, 20]      -- [11, 21]
```

---

## v0.18:  (Clojure/Lisp)

### 
- compose 
- take/drop 
- partial 

### 
```bash
# 1.  Value::Compose, Value::Partial
# src/interpreter.rs: 

# 2.  compose/partial 
# src/interpreter.rs: call_function

# 3.  take/drop 
# src/interpreter.rs: call_method

# 4. 
cargo test

# 5. 
git commit -m "feat(v0.18):  (Clojure/Lisp)"
```

### 
```mora
-- compose
let transform = compose(double, add_one)
5 |> transform    -- 11

-- take/drop
[1,2,3,4,5].take(3)   -- [1,2,3]
[1,2,3,4,5].drop(2)   -- [3,4,5]

-- partial
let add10 = partial(add, 10)
add10(5)    -- 15
```

---

## v0.19:  (Ballerina/Clojure)

### 
- Worker 
- 
- atom/swap/deref

### 
```bash
# 1.  AST 
# src/ast.rs: Worker, Send, Receive, Transaction, Commit, Rollback

# 2. 
# src/lexer.rs: worker, transaction, commit, rollback, compensation

# 3.  Parser
# src/parser.rs:  worker/transaction

# 4.  Interpreter
# src/interpreter.rs: execute_parallel_workers, execute_transaction_body

# 5.  Value::Atom
# src/interpreter.rs: atom/swap/deref

# 6. 
cargo test

# 7. 
git commit -m "feat(v0.19):  (Ballerina/Clojure)"
```

### 
```mora
-- Worker 
parallel
  worker w1
    print("worker 1")
  end
  worker w2
    print("worker 2")
  end
end

-- 
transaction
  print("in transaction")
  commit
end

-- 
transaction
  print("in transaction")
  rollback
compensation
  print("compensating")
end

-- Atom
let counter = atom(0)
swap(counter, fn(n) return n + 1 end)
deref(counter)    -- 1
```

---

## v0.20:  (Smalltalk/Common Lisp)

### 
-  (type_of/is_instance/methods_of)
- 
-  (value.rs, flow.rs, unwrap→expect)

### 
```bash
# 1. 
# src/interpreter.rs: type_of, is_instance, methods_of

# 2. 
# src/ast.rs: MacroDef
# src/lexer.rs: macro 
# src/parser.rs: 
# src/interpreter.rs: 

# 3. :  value.rs
# src/value.rs: Value, Environment, FlowSignal

# 4. :  flow.rs
# src/flow.rs:  + JSON 

# 5. : unwrap → expect
# src/interpreter.rs: 60+ 

# 6. 
cargo test

# 7. 
git commit -m "feat(v0.20):  (Smalltalk/Common Lisp)"
```

### 
```mora
-- 
type_of(42)                     -- "number"
is_instance("hello", "string")  -- true
methods_of([1,2])               -- ["push","pop","map",...]

-- 
macro when(condition, body)
  if condition then body end
end
when(x > 5, print("big"))
```

---

## 

:
```bash
# 1. 
cargo build && cargo test && cargo clippy

# 2.  TODO
grep -rn "TODO" src/ --include="*.rs"

# 3. 
cat docs/learning-plan.md
```

:
```bash
# 1. 
cargo test

# 2. 
cargo clippy

# 3. 
# CHANGELOG.md, docs/mora-spec.md

# 4. 
git commit -m "feat(v0.XX): "
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

# 
git add -A && git commit -m "feat: description"

# 
git log --oneline | grep "v0\."
```

---

*v0.11 → v0.20  — 2026-06-28*

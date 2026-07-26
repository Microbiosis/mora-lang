# ""

> ""
> **""""**

---

## 0. 

 `mora-lang` 

1. 
2. 
3. 
4. 


- **""**
- **""**
- **""**

** `.rs`  + git  +  `.md`  `.rs`  `//` **

---

## 1. 6 

```

  Phase 1:  —— /                       
  Phase 2:  #1 —— ""                  
  Phase 3:  ——  .md .rs               
  Phase 4:  #2 —— ""                    
  Phase 5:  ——  +  + grep          
  Phase 6: "" ——  5           

```

" →  → "

---

## 2. Phase 1 — 

### 2.1 

****

|  |  |
|------|---------|
| `README.md` | "README  89  `parallel ... end` " |
| `README.md` | "v0.19 Worker Ballerina`parallel worker w1 ... end end`" |
| `AGENTS.md` | " `grep` " |
| `Cargo.toml` | "version = "0.0.34"" ——  banner  v0.25  |
| `CHANGELOG.md` | "v0.34 commit `b374975`" |
| `.rs`  `//`  | "v0.22: AI ""v0.24: " |
|  | `src/typeck/mod.rs:1115-1119` "Type::Union(vec![])  boundary " |

### 2.2 

****

> "README  AI  → README "



> ".rs  `//`  commit PR review →  = "

**** 

### 2.3 

"4 "********

---

## 3. Phase 2 —  #1

### 3.1 

> "README.md "

### 3.2 

 README ****

1. ""`Cargo.toml:3` vs `main.rs:33`
2.  git log  5  `fix(v0.34)` 
3.  7 "README "

****** git  + ** README 
**** `typeck/mod.rs:1115-1119` ""

### 3.3 

> " git """

`docs/`  `.gitignore` `AGENTS.md``CLAUDE.md`  gitignore

```
$ cat .gitignore
/target
...
docs/
CLAUDE.md
AGENTS.md
```

** git ls-files ******

---

## 4. Phase 3 — 

### 4.1 

" .rs "****

1.  `src/ast_v2.rs:369-370`  "v0.19: Worker " 
2.  `src/interpreter/mod.rs:155`  "v0.19 Worker  channels" 
3.  `src/typeck/mod.rs:1115-1119`  "Type::Union(vec![])  boundary " ""
4.  `// v0.22: ` 

### 4.2 

10 " / "****

---

## 5. Phase 4 —  #2

### 5.1 

> ""

### 5.2 

****

1. ****`.md` 
2. **`.rs`  `//`  == ** commit
3. README ****

### 5.3  todo

```markdown
- [in_progress] Stop treating .md / docs/ as project documentation
- [pending] Treat inline // comments in .rs files as stale documentation
- [pending] Only derive facts from: code paths, grep, type signatures, control flow
- [pending] For each claim, show exact .rs file:line that proves it (or admit I haven't read enough)
- [pending] Never cite README, CHANGELOG, AGENTS.md, CLAUDE.md, docs/*.md, or inline // comments
```

### 5.4 

> **" grep "** —— ""

---

## 6. Phase 5 — 

### 6.1 



|  |  |  |
|------|------|------|
| **** |  +  + grep  | "execute.rs:53 `Worker { .. } => Ok(FlowSignal::None` — body " |
| **** |  /  /  | "`Interpreter::Clone` impl  clone reset" |
| **** |  /  /  | " 10K  =  GB —— " |

### 6.2 "" grep 

```bash
#  Worker body 
grep -n "Worker" src/parser_v2/ src/ast_v2.rs src/interpreter/

#  parallel 
grep -n "execute_parallel\|// " src/interpreter/execute.rs

#  HTTP worker  Interpreter 
grep -rn "Arc<Mutex<Interpreter>>" src/

#  ai_cache 
grep -n "ai_cache\." src/interpreter/ai_chat.rs

#  MORA_NO_TYPECK 
grep -rn "MORA_NO_TYPECK" src/ tests/

#  Union(vec![]) fallback 
grep -n "Type::Union(vec!\[\])" src/typeck/check.rs | wc -l

#  set_type / get_type 0 caller
grep -rn "set_type\|arena\.set_type\|arena\.get_type" src/
```

### 6.3 

********

---

## 7. Phase 6 — ""

### 7.1 

> ""

** 5 **

### 7.2 5 

1. **`Interpreter::Clone` impl **
2. **`call_value` / `call_value_inner` **
3. **`TypedExpr.ty` **
4. **method dispatch  type **
5. **Clone impl  `HashMap::new()` **

### 7.3 

#### 7.3.1 Clone impl`src/interpreter/mod.rs:230-270`



```rust
impl Clone for Interpreter {
    fn clone(&self) -> Self {
        Self {
            globals: self.globals.clone(),           // Arc::clone, 
            environment: self.environment.clone(),   // Arc::clone, 
            tool_registry: self.tool_registry.clone(),  // 
            model_routes: self.model_routes.clone(),    // 
            token_budget: self.token_budget.clone(),    // 
            token_usage: self.token_usage.clone(),      // 
            trace: self.trace.clone(),                  // 
            route_registry: self.route_registry.clone(),// 
            current_ai_config: self.current_ai_config.clone(), // 
            trait_registry: self.trait_registry.clone(),// 
            impl_table: self.impl_table.clone(),        // 
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(),   // reset
            ai_cache: HashMap::new(),           // reset
            // ...  17  reset
            v2_arena: None,                     // reset
        }
    }
}
```

****12  deep clone 2  `Arc::clone` 10  HashMap/Vec+ 17  reset

****"10K  = 10K  GB "——****`dispatch.rs:998, 1035`  ** server  clone **per-worker  cloneN  worker = N  Interpreter ** N **

#### 7.3.2 call_value `src/interpreter/dispatch.rs:1063-1101`

```rust
pub(crate) fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
    match value {
        Value::Closure { v2_node_id, .. } => {
            if v2_node_id.is_some() {
                if let Some(ref arena) = self.v2_arena.clone() {
                    return self.call_value_inner(value, args, arena);  //  &mut self  body
                }
                ...
            }
            ...
        }
        ...
    }
}
```

**`&mut self`  =  closure body**`http_server.rs:311`  `interpreter.lock().expect(...).call_value(...)` ** HTTP handler  `Mutex<Interpreter>` **

#### 7.3.3 TypedExpr.ty `src/ast_v2.rs:597-601`

```rust
pub fn set_type(&mut self, id: NodeId, ty: Type) {
    if let Some(expr) = self.exprs.get_mut(id.0) {
        expr.ty = Some(ty);
    }
}
```

`grep -rn "set_type\|arena\.set_type" src/` —— **0 caller**

`src/typeck/check.rs:741` `pub fn check_expr` ** `Type`**** `arena.set_type(expr_id, ret_ty)`**

`alloc_expr`  `ty: None`** None**

**`TypedExpr.ty` **—— `get_type`  0 caller

#### 7.3.4 method dispatch`src/interpreter/dispatch.rs:442-941`



```rust
pub(super) fn call_method(&mut self, mut object: Value, method: &str, args: Vec<Value>, call_site: Span) -> Result<Value, String> {
    let _cache_key = format!("{}:{}", type_name(&object), method);  //  _ 
    if let Value::TraitObject { .. } = &object {
        return self.dispatch_trait_method(&object, method, args, call_site);
    }
    match object {
        Value::List(list) => match method {
            "push" => { ... }
            "map" => { ... }
            // ... 30+ 
        },
        Value::Dict(map) => match method { ... },
        Value::String(s) => match method { ... },
        Value::Builtin(name) => match (name.as_str(), method) {
            ("web", "fetch") => { ... }
            ("json", "parse") => { ... }
            // ...
        },
        // ...
    }
}
```

** type table monomorphize O(1)  pattern match **

`_cache_key`  —— **""******

#### 7.3.5 Clone impl  HashMap::new() 

**17  reset**

`mod.rs:230-270`  245-267  reset 

### 7.4 

****4 " +  + "** .rs :**

---

## 8.  Phase 6 

|  |  |  |
|------|---------|---------|
| **** | `src/interpreter/execute.rs:53` Worker body `src/http_server.rs:311`  =  handler`call_value` `&mut self` | **** |
| **** | `src/interpreter/mod.rs:230-270` Clone 12  + 17 reset`src/interpreter/ai_chat.rs:463-468`  Agent`src/interpreter/dispatch.rs:1063-1101` `&mut self`  handler `src/interpreter/dispatch.rs:442-941`  match  | ******** |
| **** | `src/typeck/check.rs` 30+  `Type::Union(vec![])` fallback`src/typeck/check.rs:744` `src/ast_v2.rs:597-601` `set_type` 0 caller`src/typeck/check.rs:741` `check_expr`  AST | ** fallback +  ** |
| **** | `src/value.rs:38-142` `src/interpreter/dispatch.rs:442-941`  match  type table`src/interpreter/mod.rs:271-275` `Environment` `src/interpreter/dispatch.rs:450` `_cache_key`  | **** Python  |

---

## 9. 

### 9.1  1

**** READMECHANGELOGAGENTS.md 

****" =  = "

****
-  .md  → 
-  `cat .gitignore`  .md 
-  .md 

### 9.2  2 .rs 

**** `// v0.22: ...``// v0.19: ...`

**** commit" = "**** commit .md 

****
-  `// xxx: ...` → 
- ** +  + grep **
- ********

### 9.3  3

****" X"" Y""v0.x  v1.0  Z"

**** +  + ""

****
- "**** grep "
-  → ""
-  → :

### 9.4  4

****

**** + ""

****
- ""
- "" → 
- ****

### 9.5 

> ** grep ""**



```bash
# 1. 
grep -rn "exact_symbol" src/

# 2. 
Read src/xxx.rs:

# 3.  caller
grep -rn "function_name\|method_name" src/

# 4. 
# -  + 
# - ""
# - ""
```

---

## 10. Checklist



### 10.1 

- [ ] `cat .gitignore` — /
- [ ] `git log --oneline -20` — 
- [ ] **** `.rs`git  `.md` `//` 

### 10.2 

- [ ] `grep -rn "feature_keyword" src/` — 
- [ ]  `.rs` :
- [ ]  `//`  + 
- [ ]  caller`grep -rn "function_or_method_name" src/`
- [ ] ""—— `set_type`  0 caller = 

### 10.3 



- [ ] **** `.rs` : + 
- [ ] ****" X / Y / Z"
- [ ] ****" X  Y"

### 10.4 "" / "" / ""

- [ ] 
- [ ] 
- [ ]  /  / 
- [ ] 

---

## 11. ****

| # |  |  |  |
|---|--------|---------|---------|
| 1 |  README.md  89  / 152  /  | "README.md " |  README  |
| 2 |  CHANGELOG  |  |  `git log`  |
| 3 |  AGENTS.md / CLAUDE.md / docs/*.md | "" | `cat .gitignore`  |
| 4 |  `.rs`  `// v0.22: ...`  | "" |  `//`  |
| 5 |  `typeck/mod.rs:1115-1119` "" | "" |  ≠  |
| 6 | "10K  =  GB " | "" | `Clone` impl  |
| 7 | "`Mutex<Interpreter>` N  ≈ 1 " | "" | `call_value` `&mut self`  |
| 8 | "`TypedExpr.ty` " | "" | grep `set_type` caller = 0 |
| 9 | "method dispatch  type " | "" | `call_method`  match  |
| 10 | "Clone impl  HashMap::new() " | "" |  17  reset |

---

## 12. 

> **""" / "**
> ""——
>  `match` / `grep 0 ` / `&mut self` / `Arc::clone` 
>
> " X" = ****
> " grep  `xxx.rs:NN`  X" = ****
>  = **""**
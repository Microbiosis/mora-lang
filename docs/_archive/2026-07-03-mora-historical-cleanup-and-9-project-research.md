# Mora  & 9  — 

> ****" →  → " 2026-07-03  `sess_68c05f30-dfcb-447c-8cce-639a743be7f8`
>  Mora ** AI  Rust "" panicstubs**

---

## 0. 

```

  Part 1:   → 3  commit                    
    ·  v0.34 sandbox builtin  dispatch 
    ·  ai.tokens builtin revert        
    ·  mock.register/unregister  stubs             
    ·  parser_v2 orchestrator loop  panic            
                                                            
  Part 2: 9  + 3          
    · mini-swe-agent / CLI-Anything                          
    · AIOS / MimiClaw / OpenFugu                             
    · OpenInfer / MinerU / Headroom / Puter                  
    ·  3 Agent OS / CLI  / AI   

```

****
- **AGENTS.md ** MCP `mcp__anysearch__*`
- **brainstorming skill**  design doc 

---

## 1. Part 1

### 1.1 ""

#### 1.1.1 

 `sess_68c05f30-dfcb-447c-8cce-639a743be7f8` ****

>  URL"session link"**** ** ""

****
- `git status`  2  (`src/interpreter/dispatch.rs`  `src/interpreter/mod.rs`)
- `git diff` v0.34 sandbox builtin " dispatch "" `call_sandbox_method`"
-  git log  `32b1dc0 feat(v0.34): bus.emit/off/count builtin (integrate event module)` ****

#### 1.1.2 

```bash
ReadSessionContext(sessionId="sess_68c05f30-dfcb-447c-8cce-639a743be7f8", strategy="handoff", maxTokens=12000)
# → Tool execution timed out after 45000ms
```

**** `strategy="relevant", maxTokens=4000` **** git/

### 1.2  sandbox builtin  `v0.34-integrate`

#### 1.2.1  vs 

AGENTS.md ""

```
1.   →  src/interpreter/dispatch.rs  src/interpreter/mod.rs
2.   →  src/value.rs (Value::Builtin)
3.  builtin →  src/interpreter/builtins.rs (call_event_method )
4.   →  src/interpreter/mod.rs  mod bus_tests
```

****
- "/"
-  builtin dispatch 
-  builtin ""
- ""

#### 1.2.2  1 sandbox dispatch 

****`src/interpreter/dispatch.rs:758-761`
```rust
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
```

****
```rust
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
```

> ****Rust  match arms 

#### 1.2.3  2 `call_sandbox_method`

 `src/interpreter/builtins.rs`**** `call_event_method`  v0.34 builtin 

```rust
/// v0.34: sandbox.* — path validation + builtin allow/deny (MimiClaw + AIOS)
pub fn call_sandbox_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "mode" => {
            let policy = &self.sandbox;
            let mode = if policy.allow.iter().any(|p| p == "*") && policy.deny.is_empty() {
                "permissive"
            } else if policy.allow.is_empty() {
                "strict"
            } else {
                "custom"
            };
            Ok(Value::String(mode.to_string()))
        }
        "check_builtin" => {
            let name = args.first().map(|v| v.to_string())
                .ok_or("sandbox.check_builtin: requires builtin name as first arg")?;
            Ok(Value::Bool(self.sandbox.check_builtin(&name).is_ok()))
        }
        "check_path" => { /* similar */ }
        _ => Err(format!("sandbox.{}: unknown method", method)),
    }
}
```

****
- `&self`  `&mut self` `self.sandbox`  `&self`
- `check_builtin`  `Result<(), String>` `Bool`true = 
-  builtin  `Result<Value, String>`** panic**AGENTS.md  3 

#### 1.2.4 

AGENTS.md  3  target 
```bash
cargo build --all-targets       # 
cargo test --all                # 
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check              # 0 diff
```

> ****** 4 **CI  4  PR 

### 1.3 "" → 

****
-  9  GitHub 
- "" README
- " Mora "

#### 1.3.1 brainstorming skill 

`brainstorming` skill 

> **HARD-GATE**: Do NOT invoke any implementation skill, write any code, scaffold any project, or take any implementation action until you have presented a design and the user has approved it. This applies to EVERY project regardless of perceived simplicity.

**brainstorming ** 9 

#### 1.3.2  Skill 

 invoke  skill
- `deep-research` agent 
- `brainstorming`
- `api-and-interface-design`

> ****** invoke  skill ** brainstorming ""api-and-interface-design ""

### 1.4 README  → 

#### 1.4.1  WebFetch  README

 `WebFetch`
- `url=https://github.com/SWE-agent/mini-swe-agent`
- `prompt=... AI `

**WebFetch  prompt **
- ****
- ****" AI "
- ****""

#### 1.4.2  Agent 

 README ——""`Agent`  `subagent_type="general-purpose"`  agent  git cloneReadgrep 

**Prompt ** agent 
```
1. Clone / fetch the repo
2. Read main source files (not just README)
3. Extract implementation principles: <>
4. Show concrete code snippets with file:line references
5. Propose 2-3 concrete Mora language primitives
Output a structured report in Chinese with sections: , , 
/, ,  Mora .
```

** 9  agent**
-  4  agent CLI-Anything / AIOS / MinerU / Headroom "Agent was cancelled before the subagent returned findings"
- ****—— 4 
-  5 

#### 1.4.3  → 3 

 9 ** Mora **

|  |  | Mora  |
|---|---|---|
| mini-swe-agent | bash  |  shell.exec /  Conversation |
| CLI-Anything | CLI `--json`SKILL.md |  harness /  skill |
| AIOS | LLM syscall  |  |
| MimiClaw | Markdown cron + heartbeat | soul.md / memory file  |
| OpenFugu | hidden state Conductor DAG |  worker |
| OpenInfer | Rust+CUDA TokenEvent |  HTTP  |
| MinerU | effort |  backend |
| Headroom | ContentRouterCCRCacheAligner |  |
| Puter |  OSAI//DB/Serverless |  driver / namespace / sandbox |

** 3 **" A/B/C + "
- **A. Agent OS **AIOS + MimiClaw + mini-swe-agent→ Capability / syscall / Agent / context / tool / schedule / heartbeat / Conversation
- **B. CLI **CLI-Anything + mini-swe-agent→ shell.exec / harness / skill / session / workspace.solve / preview
- **C. AI **OpenFugu + OpenInfer + Headroom→ route / TokenEvent / engine / compress.route / ccr / pipeline

****
1. ****
2. ** Mora **
3. ****

### 1.5 brainstorming skill 

 B → C → A brainstorming skill ****

****——

### 1.6 ""

****

```bash
git status                                     # 
cargo build --all-targets                       # 
cargo test --all                                # 
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check                              # 0 diff
#  examples/  .mora
# grep TODO/FIXME/unimplemented!()
# git log --oneline --all --grep="Revert"
#  CHANGELOG ""
```

**"×"** 1 

#### 1.6.1 ——`ai.tokens`  revert

```bash
git log --oneline --all --grep="Revert"
# 92355d8 Revert "feat(v0.34): ai.tokens builtin (mini-swe-agent cost tracking)"
```

`git show 374570e` revert  commit
- ** dispatch **`("ai", "tokens") => ...` 
-  `("ai", "tokens")` " dispatch + method " `ai.tokens.input()`  `ai.tokens`  `.input()` `ai.tokens.tokens: unknown method`

****—— builtin
```rust
("ai", "tokens") => Ok(Value::Builtin("ai.tokens".to_string())),
("ai.tokens", method) => self.call_ai_tokens_method(method, &args),
```

**** "Revert" commit **** `git show`  commit —— buggy  revert bug

#### 1.6.2 

```bash
git commit -m "fix(v0.34): re-implement ai.tokens builtin with nested dispatch" \
           -m "The original implementation used a duplicate dispatch arm..." \
           -m "..."
```

**** commits
-  `<type>(<scope>): <subject>` —— scope  v0.34
-  `-m`  ** why

### 1.7  mock.register/unregister 

CHANGELOG "mock.register is a stub"`src/interpreter/builtins.rs:437-460`  `call_mock_method`
```rust
"register" => {
    let name = ...;
    Ok(Value::String(format!("mock.{} registered", name)))  // 
}
```

#### 1.7.1 MockHandler 

 `MockHandler`  `Arc<dyn Fn>`  `Value`Mora ** Rust API **

**** handler
```rust
pub enum MockHandler {
    /// Rust  handler Rust 
    Native(Arc<dyn Fn(&Value) -> Value + Send + Sync + 'static>),
    /// Mora 
    Script(Value),
}
```

**`#[derive(Debug)]` **`Arc<dyn Fn>`  `Debug`
```rust
impl std::fmt::Debug for MockHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MockHandler::Native(_) => f.debug_tuple("Native").finish(),
            MockHandler::Script(v) => f.debug_tuple("Script").field(v).finish(),
        }
    }
}
```

#### 1.7.2 `call_value`  — 

 `MockHandler::Script(closure)` **** `self.call_value(&closure, vec![args])`
-  v2  `v2_arena`
- 
-  `call_mock_method`  `&mut self` `&self` `call_value` 

#### 1.7.3  borrow  get 

```rust
"call" => {
    let name = ...;
    let call_args = args.get(1).cloned().unwrap_or(Value::Nil);
    match self.mock_registry.get(&name) {  //  Option<MockHandler>
        Some(MockHandler::Native(f)) => Ok(f(&call_args)),
        Some(MockHandler::Script(closure)) => {
            self.call_value(&closure, vec![call_args])  //  &mut self
        }
        None => Ok(Value::Nil),
    }
}
```

`get`  `Option<MockHandler>`clone  owned value**** `self.call_value` —— `&self`  `&mut self` 

#### 1.7.4 Mora  e2e 

```mora
let handler = fn(x) return x * 2 end
mock.register("double", handler)
let doubled = mock.call("double", 21)   //  42
let n2 = mock.count()                    // 1
mock.unregister("double")
```

**** + call  Mora  + unregister 

### 1.8  parser_v2 orchestrator loop  panic

`src/parser_v2/statements.rs:853`:
```rust
let agent = agents.into_iter().next()
    .expect("loop requires exactly one agent");  // ←  panic
```

** line 868-875 ""**—— panic `eprintln!` + 
```rust
_ => {
    eprintln!("Parse error: Expected 'sequential', 'graph', or 'loop', got '{}'", mode);
    OrchestrateKind::Sequential { agents: Vec::new() }
}
```

****
```rust
let agent = match agents.into_iter().next() {
    Some(a) => a,
    None => {
        eprintln!("Parse error: orchestrate loop requires exactly one agent");
        return StmtKind::Orchestrate { /*  */ };
    }
};
```

#### 1.8.1  `panic!` 

```python
#  panic!  TEST  PROD
import re
for f in files:
    in_test = False
    for line in lines:
        if re.search(r'#\[test\]', line): in_test = True
        if re.search(r'panic!', line):
            print(f'{f}:{i}: [{"TEST" if in_test else "PROD"}] {line}')
```

**12  `panic!`  `#[cfg(test)]` **—— `parser_v2/statements.rs:853`  `.expect` 

> **** `panic!` **** `#[cfg(test)]`AGENTS.md  `unwrap/panic`

### 1.9 "///"

****

|  |  |  |
|---|---|---|
|  |  +  `Arc<Mutex<>>` |  |
|  | eprintln  |  |
|  | typeck  builtin  `Type::Union(vec![])`  |  |
|  | harness/skill  builtin  |  |

**** AGENTS.md " panic""v0.x  breaking change"

** 4 **
1. `parser_v2`  `Result` 
2. typeck  builtin 
3. `Arc<Mutex<>>`  `DashMap` / sharded lock
4.  +  arena

****"3  fix"3 commits""

---

## 2. Part 29 

>  agent **** README 

### 2.1 mini-swe-agentSWE-agent
- ****`Environment.execute(action) -> {output, returncode, ...}` +  `self.messages` 
- ****"no tools other than bash" —  LLM " shell "
- ****`shell.exec` / `ShellResult` + `Conversation` / `Turn` + `render`
- ****`src/minisweagent/environments/local.py:24``src/minisweagent/agents/default.py:88-122`

### 2.2 CLI-AnythingHKUDS
- ****`harness`  CLI + `SKILL.md`  + `--json`  + `cli-hub` 
- ****"Use the real software" —  backend
- ****`harness` / `skill` / `session` undo/redo/ `preview` 
- ****`cli-anything-plugin/skill_generator.py``cli-anything-plugin/repl_skin.py``cli-hub/cli_hub/registry.py`

### 2.3 AIOSagiresearch
- ****`Syscall`  `Thread` + `Event``Query`  LLM/Memory/Storage/Tool
- **** LLM  OS 
- ****`Capability<T>`  + `syscall`  + `Agent { ... }` + `spawn`
- ****`aios/syscall/syscall.py:55-69``aios/scheduler/fifo_scheduler.py:206`

### 2.4 MimiClawmemovai
- ****FreeRTOS  + `context_build_system_prompt()`  `SOUL.md/USER.md/MEMORY.md` + `cron.json`  + `heartbeat`  `HEARTBEAT.md`
- **** OS  ESP32-S3  agent loop
- ****`context { soul, user, memory }` + `tool { name, schema, handler }` + `schedule` / `heartbeat` 
- ****`main/agent/context_builder.c:28-103``main/cron/cron_service.c:241-299``main/heartbeat/heartbeat.c:31-73`

### 2.5 OpenFugutrotsky1997
- ****Qwen3-0.6B  hidden state →  workerConductor  DAG  worker 
- **** worker  19.5K  coordinator
- ****`route.select(state, pool, mask)` + `workflow { step, agent, access }` + `evolve` / `cma-train`
- ****`openfugu/mini.py:39-45`VEC_LEN `openfugu/ultra.py:86-90`parse_workflow

### 2.6 OpenInferopeninfer-project
- ****`EngineHandle` + `GenerateRequest` + `TokenEvent` mpsc  + per-request tag
- **** Rust+CUDAfeature-gated per-model crate
- ****`engine.load()` + `kv_cache`  + `@cuda_graph` 
- ****`openinfer-engine/src/engine.rs:68-170``openinfer-kv-cache/src/pool.rs:319-329`prefix matching

### 2.7 MinerUopendatalab
- ****pipeline / vlm / hybrid `middle_json``effort=medium/high` 
- ****Backend Adapter +  + 
- ****`document.parse(path, backend=, effort=, window=)` +  `Document` 
- ****`mineru/backend/hybrid/hybrid_analyze.py:83`MEDIUM_EFFORT 

### 2.8 Headroomheadroomlabs-ai
- ****`ContentRouter`  → `CcrStore`  + `<<ccr:HASH>>` `PipelineStage` 11 
- ****""
- ****`compress.route(content)` + `ccr<T>`  + `pipeline { stage ... }`
- ****`crates/headroom-core/src/transforms/content_detector.rs:221-255``crates/headroom-core/src/ccr/mod.rs:72-86`hash 

### 2.9 PuterHeyPuter
- **** iframe + `postMessage` `DriverController`  `/drivers/call``SystemKVStore`  actor+app 
- **** OS  Node.js  AI//DB/Serverless
- ****`driver::<interface>.<method>` + `namespace { ... }` + `sandbox { ... }` 
- ****`src/backend/drivers/ai-chat/ChatCompletionDriver.ts` fallback`src/puter-js/src/modules/KV.js`

---

## 3. 

>  +  Mora  + 

### 3.1  AAgent OS 

```mora
let llm = capability(LLM, { models: ["openai/gpt-4o"], budget: { max_tokens: 4096 } })
let resp = syscall "researcher" -> llm({ messages: [...] })
let researcher = Agent { name: "researcher", capabilities: [llm, mem], time_slice: 1.0s, context: ctx }
let pid = spawn researcher.run(task: "Summarize AIOS paper")
let ctx = context { soul: file("SOUL.md"), user: file("USER.md"), memory: file("MEMORY.md") }
let web_search = tool { name: "web_search", params: { query: string }, handler: fn(q) => ... }
schedule every 3600 { message: "Summarize notes" }
heartbeat monitor "HEARTBEAT.md" every 1800
let convo = Conversation(); convo.add_system(render(...)); ...
```

**6 ** commit

### 3.2  BCLI 

```mora
let r = shell.exec("ls -la", timeout: 30)
let or = harness { name: "openrefine", entry: "cli-anything-openrefine", json: true }
or.run("project list")
skill.load("skills/openrefine/SKILL.md")
skill.call("openrefine.project.list")
let s = session { path: "run.json", autosave: true }
workspace.solve("fix bug", { repo: ".", test: "cargo test" })
```

**6 **

### 3.3  CAI 

```mora
let pool = model_pool [{ name: "gpt-4o", cost: 10, ... }, ...]
let m = route.select("", pool, strategy: "cost", tags: ["reasoning"])
let stream = ai.submit({ messages: [...], max_tokens: 256 })
for event in stream { match event { TokenEvent.Token{t} => print(t) ... } }
let compressed = compress.route(text)
let c = ccr.compress(long_json, strategy: smart_crush)
```

**4 **

---

## 4. 

""

### 4.1  skill
1. **using-superpowers** —  skill 
2. **brainstorming** — harness 
3. **finishing-a-development-branch** — 4 
4. **api-and-interface-design** —  + Hyrum 

### 4.2 
1. **AGENTS.md** — unwrap  /  4  cargo  / CHANGELOG 
2. **Cargo.toml** — 
3. **src/lib.rs** — 
4. **CHANGELOG.md** — 

### 4.3 
- `Bash` (`git`, `cargo` )
- `Read` / `Edit` / `Write` 
- `Grep` / `Glob` 
- `Agent`  agent 
- `WebFetch`  README
- `ReadSessionContext` 
- `Skill` invoke skill 

### 4.4  Rust 
- `Arc<Mutex<>>`  +  `clone().get()` 
- `derive(Debug)`  +  `impl Debug`
- `&self` vs `&mut self` 
- `Result<T, E>`  `panic!` 
- `Option<T>`  match 

---

## 5. 5 

### 5.1 """"

""—— ** `git status` 

### 5.2  =  README 

" README """**''''**—— README 

### 5.3 brainstorming  3  **  ** 

A/B/C **** A  B

### 5.4 "" ** ** /

 3  fix `eprintln`  / `Value::Builtin`  / `match`  `Result`****""""""

### 5.5  = ( panic) + (stub builtin) + ( module) + ( API)

**panic > stub >  > ** panic stub  builtin "" 0→1 1→

---

## 6.  4  commit 

| SHA |  |  |
|---|---|---|
| `f1a366e` | fix(v0.34) | re-implement ai.tokens builtin with nested dispatch |
| `ba1bcd1` | fix(v0.34) | mock.register/unregister actually wire handlers |
| (pending) | fix(v0.34) | parser_v2 orchestrator loop no longer panics |

 4  panic  commit 

---

## 7. 

```bash
# 1. 
cd D:/Github/mora-lang
git checkout main
git pull

# 2. 
cargo build --all-targets && cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check

# 3.  Revert commit
git log --oneline --all --grep="Revert"

# 4.  commit
git show <original-sha>

# 5. 
cargo build --all-targets 2>&1 | head -50

# 6.  +  + commit
# ... 
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
git add <files> && git commit

# 7. 
#  Agent  agent
# "Read the main source files of <repo> and propose 2-3 Mora primitives"

# 8.  design doc
# brainstorming skill  docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md
```

---

## 8. ""AI 

- ****
- ****v0.x  breaking change
- ****""""
- ****A/B/C 
- **** commit message / CHANGELOG / design doc " simple implfuture work: ..."

---

** 4  commit  git log**

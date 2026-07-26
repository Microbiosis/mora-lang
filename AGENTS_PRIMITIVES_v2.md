# Mora v0.34+  v2 — mini-swe-agent + CLI-Anything 

> ****:  deep-dive 2  AI  (mini-swe-agent, CLI-Anything) ,
> **** README,  Mora .
>
> ****:  clone ,  .py  ( README), ********
>
> ** AGENTS_PRIMITIVES.md (v1)**: v1  7  AI  (AIOS/MimiClaw/
> OpenFugu/OpenInfer/MinerU/Headroom/Puter) **/**. v2  2 **
> AI ****/**.

---

## 0.  (One-liner)

> v1 "" (mora  AI//). v2 "****":
> exceptions-as-flow3-mode multi-layer source fallbackTTL cache
> fallbackabort_exceptions interrupt taxonomy. Mora ****,
> ****.

---

## 1. mini-swe-agent 

`https://github.com/SWE-agent/mini-swe-agent` —— 3 :
- `agents/default.py` (188 ): step/query loop
- `environments/local.py` (92 ): bash executor
- `models/litellm_model.py` (163 ): LLM 

### 1.1 exceptions-as-flow 

****: `agents/default.py:100-117` + `exceptions.py`

****:  = messages  `role: "exit"` ,  Python 

```python
# default.py:100
except FormatError as e:
    self.n_consecutive_format_errors += 1
    if 0 < self.config.max_consecutive_format_errors <= self.n_consecutive_format_errors:
        self.add_messages(
            *e.messages,
            {"role": "exit", "content": "RepeatedFormatError", ...},
        )
    else:
        self.add_messages(*e.messages)
except InterruptAgentFlow as e:
    self.add_messages(*e.messages)
except Exception as e:
    self.handle_uncaught_exception(e)
    raise  # fatal,  raise
```

**5  interrupt taxonomy** (`exceptions.py`):
- `FormatError` — LLM 
- `InterruptAgentFlow` —  ()
- `LimitsExceeded` — step/cost 
- `TimeExceeded` — wall time 
- `Submitted` —  (sentinel string )
- `UserInterruption` —  interactive mode

**Mora **: Mora  `Result<Value, String>` **** interrupt taxonomy`src/interpreter/mod.rs:556-591`  `run_repl_with`  `?` **" → messages "**

**Mora  (P0)**:

```mora
// v0.34: Interrupt primitive (5 )
interrupt FormatError { message: String, response: Value }
interrupt LimitsExceeded { kind: String, current: number, limit: number }
interrupt TimeExceeded { elapsed_s: number, limit_s: number }
interrupt Submitted { output: String }
interrupt UserInterruption { kind: String, comment: String }

// builtin: emit interrupt  messages 
bus.emit("interrupt." + name, payload)
```

### 1.2 3-mode  (human/confirm/yolo)

****: `agents/interactive.py:25-29, 165-182`

****:  agent, 3 ****:
- `human`:  ( LM)
- `confirm`: LM  (`y`  / `/u`  human)
- `yolo`: LM  (, CI )

**Whitelist **: `whitelist_actions: list[str]` `rm -rf /`  → 

```python
# interactive.py:162-163
def _should_ask_confirmation(self, action: str) -> bool:
    return self.config.mode == "confirm" and not any(
        re.match(r, action) for r in self.config.whitelist_actions
    )
```

**CI-safe stdin check** (interactive.py:97-107): `sys.stdin.isatty()` , CI `stdin = /dev/null` prompt  `EOFError`

**Mora **: Mora v0.33  `SandboxPolicy`  `allow/deny`** user interaction**Script ""

**Mora  (P1)**:

```mora
// v0.34: sandbox run mode (3-mode like interactive.py)
let result = sandbox.run(script, {
    mode: "confirm",         // "human" | "confirm" | "yolo"
    whitelist: ["^ls", "^cat"],  // 
    stdin_tty_check: true,    // CI  (stdin=/dev/null  prompt)
})

// builtin: interrupt for user confirmation
if interrupt? then
    let choice = interrupt.comment  // "y" | "/u" | user comment
    handle_user_decision(choice)
end
```

### 1.3  limits 

****: `agents/default.py:130-145` + `AgentConfig` (default.py:19-35)

```python
# AgentConfig
step_limit: int = 0           # 0 = no limit
cost_limit: float = 3.0       # 
wall_time_limit_seconds: int = 0

# query() 
if 0 < self.config.step_limit <= self.n_calls or
   0 < self.config.cost_limit <= self.cost:
    raise LimitsExceeded(...)
if 0 < self.config.wall_time_limit_seconds <= int(time.time() - self._start_time):
    raise TimeExceeded(...)
```

****:  limits ****,  exception, ****`step_limit == 0` **default to permissive**

**Mora **: Mora `ai_infra::TokenBudget`  `step_limit` ****`#[allow(dead_code)]`Mora  cost_limit / wall_time_limit 

**Mora  (P0)**:

```mora
// v0.34:  limits block
ai.limits({
    step: 100,              //  100 
    cost: 3.0,              //  $3
    wall_time_s: 600,       //  10 
}) {
    let answer = ai.chat(p"...")
    // :  step  LimitsExceeded(step)
    //           cost  LimitsExceeded(cost)
    //           wall_time  TimeExceeded
}
```

### 1.4 abort_exceptions  (retry )

****: `models/litellm_model.py:50-57` + `models/utils/retry.py`

```python
# litellm_model.py:50
abort_exceptions: list[type[Exception]] = [
    litellm.exceptions.UnsupportedParamsError,
    litellm.exceptions.NotFoundError,
    litellm.exceptions.PermissionDeniedError,
    litellm.exceptions.ContextWindowExceededError,
    litellm.exceptions.AuthenticationError,
    KeyboardInterrupt,
]

# retry.py: 14
return Retrying(
    reraise=True,
    stop=stop_after_attempt(int(os.getenv("MSWEA_MODEL_RETRY_STOP_AFTER_ATTEMPT", "10"))),
    wait=wait_exponential(multiplier=1, min=4, max=60),
    before_sleep=before_sleep_log(logger, logging.WARNING),
    retry=retry_if_not_exception_type(tuple(abort_exceptions)),
)
```

****: `retry_if_not_exception_type(tuple(abort_exceptions))` —— **abort **`UnsupportedParamsError` / `NotFoundError` / `PermissionDeniedError` / `ContextWindowExceededError` / `AuthenticationError` / `KeyboardInterrupt` ****

**Mora **: Mora `src/interpreter/mod.rs:73-99` `is_retryable_error()` network/429/5xx ** abort_exceptions**

**Mora  (P2)**:

```rust
// src/interpreter/ai_chat.rs
const ABORT_EXCEPTIONS: &[&str] = &[
    "auth", "permission", "not_found", "context_window", "quota",
];
// retry  check
if error_msg.contains_any(ABORT_EXCEPTIONS) {
    return Err(error);  // 
}
```

### 1.5 sentinel string submit

****: `environments/local.py:45-56`

```python
def _check_finished(self, output: dict):
    lines = output.get("output", "").lstrip().splitlines(keepends=True)
    if lines and lines[0].strip() == "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT" and output["returncode"] == 0:
        submission = "".join(lines[1:])
        raise Submitted(...)
```

****: bash ****  sentinel string ****—— `is_done()` **output **  submission

**Mora **: Mora `mcp_server`  `Value::Dict`  sentinel string****:  (`Dict`/`List`/`String`)""

**Mora  (P1)**:

```mora
// v0.34: mcp tool  sentinel
tool.shell("run_tests")  //  "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT\nall 12 tests passed"
// builtin :  = sentinel,  = submission
mcp.submit(submission)  //  Submitted interrupt
```

### 1.6  kill 

****: `environments/local.py:84, 89`

```python
process = subprocess.Popen(
    command, shell=True, text=True, cwd=cwd, env=env, ...,
    start_new_session=os.name == "posix",  #  session (process group)
)
try:
    stdout, _ = process.communicate(timeout=timeout)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL) if os.name == "posix" else process.kill()
    stdout, _ = process.communicate()
    raise subprocess.TimeoutExpired(command, timeout, output=stdout)
```

****: `start_new_session=True` (POSIX)  process group, timeout  `os.killpg` ****, 

**Mora **: Mora  `shell` builtinv0.20  v0.33 sandbox policy  path validation  process group kill

**Mora  (P2)**:

```mora
// v0.34: shell.run 
let result = shell.run("make test", {
    timeout_s: 30,
    kill_process_group: true,  // POSIX 
})
```

### 1.7 FormatError MUST  response (spec contract)

****: `models/litellm_model.py:88-97`

```python
try:
    actions = self._parse_actions(response)
except FormatError as e:
    try:
        e.messages[0]["extra"]["response"] = response.model_dump(mode="json")
    except Exception:
        e.messages[0]["extra"]["response"] = repr(response)
    raise
```

****:  parse , response **** Spec contract —  trajectory ,  LLM 

**Mora **: Mora `record`  AI call `src/record/`**** parse 

**Mora  (P2)**:

```mora
// v0.34: record  parse 
record.config({on_error: "persist"})
let response = ai.chat(p"...")  //  JSON parse , response 
```

### 1.8 OpenAI  tool schema ( tool `bash`)

****: `models/utils/actions_toolcall.py:8-23`

```python
BASH_TOOL = {
    "type": "function",
    "function": {
        "name": "bash",
        "description": "Execute a bash command",
        "parameters": {
            "type": "object",
            "properties": {"command": {"type": "string", "description": "..."}},
            "required": ["command"],
        },
    },
}
```

****: ** tool `bash`**,  Read/Edit/Glob  tool——****,  LLM 

**Mora **: Mora `tool_def`  tool, **** `mcp_server`  OpenAI  JSON schema****

**Mora  (P3)**: , .

### 1.9 Pydantic config + `model_dump(mode="json")` 

****:  codebase,  `LitellmModelConfig(BaseModel)` (litellm_model.py:27)

****: Pydantic BaseModel + `model_dump(mode="json")`  JSON-safe

**Mora **: Mora  Pydantic ( Python)`Serialize`  `serde::Serialize` Rust  **Mora **——value.rs  derive Serialize

**Mora  (P3)**:  Value enum  `Serialize` + `Deserialize` derive.  v0.31 "0 "`serde_json`  transitive ( `undoc`) ** derive** `serde::Serialize` derive for Value dependency

---

## 2. CLI-Anything 

`https://github.com/HKUDS/CLI-Anything` ——  GUI  skill
 `cli-hub/cli_hub/` (4931  8 ):

- `registry.py` (117):  registry  + TTL cache
- `matrix.py` (537): matrix 
- `matrix_skill.py` (397): 4  source fallback  SKILL.md
- `installer.py` (604): npm/uv 
- `cli.py` (1030): CLI 

### 2.1  registry + 3  cache fallback

****: `registry.py:32-90` + `matrix.py:48-80`

****: network → cache → local file (3  fallback):

```python
# registry.py:32 _fetch_json
def _fetch_json(url, cache_file, force_refresh=False):
    _ensure_cache_dir()
    if not force_refresh and cache_file.exists():
        try:
            cached = json.loads(cache_file.read_text())
            if time.time() - cached.get("_cached_at", 0) < CACHE_TTL:
                return cached["data"]
        except (json.JSONDecodeError, KeyError):
            pass
    try:
        resp = requests.get(url, timeout=15)
        resp.raise_for_status()
        data = resp.json()
    except (requests.RequestException, ValueError):
        cached_data = _load_cached_data(cache_file)
        if cached_data is not None:
            return cached_data  # ←  stale cache 
        raise
    cache_payload = {"_cached_at": time.time(), "data": data}
    cache_file.write_text(json.dumps(cache_payload, indent=2))
    return data
```

****: 2nd fallback (stale cache on network error) ** raise **

**Mora **: Mora `mcp_server`  registry `mcp_server.rs`  hardcode 

**Mora  (P1)**:

```rust
// v0.34: Registry  + fallback
pub struct Registry {
    entries: Arc<Mutex<HashMap<String, RegistryEntry>>>,
    cache_path: Arc<Mutex<Option<PathBuf>>>,
    cache_ttl_s: u64,
}

impl Registry {
    pub fn load(&self, source_url: &str) -> Result<Vec<RegistryEntry>, String> {
        // 1. cache file (if fresh)
        // 2. network
        // 3. cache file (stale) — never raise without trying
        // 4. local file fallback
    }
}
```

### 2.2 multi-layer source fallback (4 )

****: `matrix_skill.py:152-171` `_resolve_matrix_content_source`

```python
def _resolve_matrix_content_source(matrix_item):
    # 1. Repo checkout (via skill_md path)
    skill_ref = matrix_item.get("skill_md")
    if skill_ref and "://" not in skill_ref and not skill_ref.startswith("npx "):
        repo_root = _find_repo_root()
        if repo_root is not None:
            candidate = repo_root / skill_ref
            if candidate.exists():
                return candidate, candidate.parent

    # 2. Bundled package data
    bundled = BUNDLED_MATRIX_DATA_DIR / matrix_item["name"] / "SKILL.md"
    if bundled.exists():
        return bundled, bundled.parent

    # 3. None → caller falls back to published URL
    return None, None
```

****: 4  source chain —— checkout → bundled → published URL → generated stub fallback ****last  stub raise

**Mora **: Mora `document` backend **** backend hardcode path`mcp_server`  hardcode

**Mora  (P2)**:

```mora
// v0.34: skill loader 4  source
let skill = skill.load("./skills/greet.md", {
    sources: [
        skill.source_checkout,    // ./skills/greet.md
        skill.source_bundled,     // ~/.mora/skills/greet.md
        skill.source_published,   // https://...
        skill.source_stub,        //  placeholder
    ]
})
```

### 2.3 `_find_repo_root` git + parent walk

****: `matrix_skill.py:45-65`

```python
def _find_repo_root():
    # 1. git rev-parse (dev mode detection)
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0:
            root = Path(result.stdout.strip())
            if root.is_dir():
                return root
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    # 2. Fallback: walk up from this file looking for .git
    current = Path(__file__).resolve().parent
    for parent in [current] + list(current.parents):
        if (parent / ".git").exists():
            return parent

    return None
```

****: git first ( git ), then  walk ( git  work)****

**Mora **: Mora `mcp_server`  hardcode path**** dev/installed 

**Mora  (P3)**:

```rust
// v0.34: dev 
pub fn find_mora_root() -> Option<PathBuf> {
    // 1. git rev-parse
    Command::new("git").args(["rev-parse", "--show-toplevel"])
        .output().ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
    // 2. parent walk
        .or_else(|| walk_up_for_dot_git(std::env::current_dir().ok()?))
}
```

### 2.4 stable prefix constant ()

****: `matrix.py:25`

```python
HARNESS_PREFIX = "cli-anything-"  #  harness CLI 
```

****: `_provider_installed` (matrix_skill.py:270)  `aliases = {name, name.removeprefix(HARNESS_PREFIX)}` 

**Mora **: Mora `ai.*` builtin  5  (`ai_chat.rs`, `builtins.rs`, `ai_helpers.rs`, `orchestrate.rs`, `main.rs`)****

**Mora  (P3)**:

```rust
// v0.34: src/builtins_prefix.rs 
pub const AI_BUILTIN_PREFIX: &str = "ai.";
pub const MEMORY_BUILTIN_PREFIX: &str = "memory.";
pub const FILE_BUILTIN_PREFIX: &str = "file.";
// builtin : rust  + builtin 
```

### 2.5 stable short labels (UI )

****: `matrix.py:31-42` `KIND_LABELS`

```python
KIND_LABELS = {
    "harness-cli": "harness",
    "public-cli": "public",
    "python": "python",
    "native": "native",
    "api": "api",
    "agent-skill": "skill",
    "agent-native": "native",
    "web-search": "web",
}
```

****:  (`harness-cli`) → UI  (`harness`)**1-1  dict**

**Mora **: Mora `Value::Display`  ad-hoc per-variant (`<http_request POST /a>`)**** short-label map

**Mora  (P3)**:

```rust
// v0.34:  short label
pub const BUILTIN_LABELS: &[(&str, &str)] = &[
    ("ai.chat", "ai"),
    ("memory.store", "mem"),
    ("file.read", "fs"),
    // ...
];

pub fn short_label(builtin: &str) -> &str {
    BUILTIN_LABELS.iter()
        .find(|(k, _)| builtin.starts_with(k))
        .map(|(_, v)| *v)
        .unwrap_or(builtin)
}
```

### 2.6 multi state file separation

****: `installer.py:13-15` ( `Path` )

```python
INSTALLED_FILE = Path.home() / ".cli-hub" / "installed.json"
MATRIX_STATE_FILE = Path.home() / ".cli-hub" / "matrix_state.json"
```

****: **** state file: `installed.json` (installed CLIs), `matrix_state.json` (matrix )

**Mora **: Mora `record/`  `recorder.jsonl` (JSONL )`~/.mora_schedule.json` (v0.33 schedule) ** builtin  state file** 

**Mora  (P3)**:  pattern, . .

### 2.7 `_copy_matrix_assets`  + ignore_patterns

****: `matrix_skill.py:174-190`

```python
def _copy_matrix_assets(content_dir, output_dir):
    copied = []
    for subdir in MATRIX_ASSET_SUBDIRS:  # ("references", "scripts")
        source = content_dir / subdir
        destination = output_dir / subdir
        if destination.exists():
            shutil.rmtree(destination)  # ← 
        if source.is_dir():
            shutil.copytree(source, destination, ignore=_COPY_IGNORE)
            copied.append(subdir)
    return copied
```

****:  + ignore pattern ( `__pycache__`, `*.pyc`, `*.pyo`)**idempotent re-install**

**Mora **: Mora `document` backend  docx  resources , **MCP tool install** 

**Mora  (P3)**:

```rust
// v0.34: mcp tool install — clean reinstall
fn install_tool(name: &str, force: bool) -> Result<PathBuf, String> {
    let dest = tools_dir().join(name);
    if dest.exists() && force {
        std::fs::remove_dir_all(&dest)?;  // 
    }
    // copy from source, excluding __pycache__...
}
```

### 2.8 kind registry pattern (3-way filter)

****: `matrix.py:23-25, 537+`

```python
AGENT_INSTALLABLE_KINDS = {"agent-skill"}
INSTALLABLE_KINDS = {"harness-cli", "public-cli"}

#  `if kind in AGENT_INSTALLABLE_KINDS` 
def render_matrix_skill_file(matrix_item, installed=None):
    for cli in ...:
        if cli["kind"] not in {"harness-cli", "public-cli"}:
            return False  #  CLI kind
```

****: kind registry ** set** ** filter **** `kind` **

**Mora **: Mora `tool_def`  kind (`builtin`)`mcp_server`  kind 

**Mora  (P2)**:

```rust
// v0.34: tool kind registry
pub const TOOL_KIND_BUILTIN: &str = "builtin";     // mora 
pub const TOOL_KIND_SHELL: &str = "shell";         // shell command
pub const TOOL_KIND_HTTP: &str = "http";           // HTTP API
pub const TOOL_KIND_SKILL: &str = "skill";         // markdown 
pub const TOOL_KIND_NATIVE: &str = "native";       // binary
```

### 2.9 TTL cache + timestamp 

****: `registry.py:39, 54-55`

```python
if time.time() - cached.get("_cached_at", 0) < CACHE_TTL:
    return cached["data"]
# ...
cache_payload = {"_cached_at": time.time(), "data": data}
```

****:  wrapper  **`_cached_at` **,  mtime

**Mora **: Mora  registry 

**Mora  (P1)**:  2.1.

### 2.10 multi package manager abstraction

****: `installer.py:53-64`

```python
def _find_npm():
    return shutil.which("npm")
def _find_uv():
    return shutil.which("uv")
```

****: `shutil.which`  package manager binary  PATH,  None

**Mora **: Mora  package manager  (Mora ****)

**Mora  (P3)**:

```mora
// v0.34: package manager  builtin ()
let npm = shell.which("npm")   //  npm  PATH 
let uv = shell.which("uv")
//  nil if not found
```

---

## 3. Mora v0.34+  v2

> v1 (AGENTS_PRIMITIVES.md)  7  AI  21  ().
> v2 ()  2  AI  **** (process), ********.

### 3.1 P0  ( v0.30-0.33  module)

|  |  |  |  |
|---|---|---|---|
| **Integrate `event::EventBus` as builtin** | mini-swe-agent exception-as-flow | `src/interpreter/builtins.rs` | `bus.emit(name, payload)` builtin  |
| **Integrate `sandbox::SandboxPolicy` as builtin** | mini-swe-agent whitelist | `src/interpreter/builtins.rs` | `sandbox.run(script, {allow, deny})` builtin |
| **Integrate `ccr::CcrStore` as builtin** | Headroom CCR ( v0.33) | `src/interpreter/builtins.rs` | `ccr.put(data) -> hash`, `ccr.get(hash) -> data` |
| **Integrate `schedule::Scheduler` as builtin** | MimiClaw cron ( v0.33) | `src/interpreter/builtins.rs` | `schedule.add(name, kind, msg, interval) -> id` |
| **`ai.limits({step, cost, wall_time})` block** | mini-swe-agent AgentConfig | `src/interpreter/ai_chat.rs` | 3  limit , interrupt  messages |
| **Interrupt primitive 5 ** | mini-swe-agent exception-as-flow | `src/interpreter/mod.rs` | `interrupt FormatError/LimitsExceeded/TimeExceeded/Submitted/UserInterruption` |
| **`shell.run` ** | mini-swe-agent killpg | `src/interpreter/builtins.rs` | `shell.run(cmd, {killpg: true, timeout_s: 30})` |
| **Registry  + 3  fallback** | CLI-Anything _fetch_json | `src/mcp_server.rs` ( module `src/registry.rs`) | network → stale cache → local file |

### 3.2 P1  ()

|  |  |  |
|---|---|---|
| **`sandbox.run({mode: "human"|"confirm"|"yolo"})`** | mini-swe-agent 3-mode |  prompt  |
| **`COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` sentinel** | mini-swe-agent _check_finished | mcp tool  = sentinel →  Submit interrupt |
| **ToolError ** | CLI-Anything _format_requires | `{kind, requires, notes, status}`  schema |
| **`mcp.tool.list()` returns TTL-cached** | CLI-Anything registry |  +  |
| **`mcp.tool.install(name)` with local checkout fallback** | CLI-Anything matrix install | 4  source: checkout → bundled → published → stub |
| **`retry` decorator builtin** | mini-swe-agent tenacity | `retry.with({abort: [...], max: 10, backoff: "exponential"}) { ... }` |
| **InterruptAgentFlow ** | mini-swe-agent exceptions | 5  interrupt taxonomy |

### 3.3 P2  ()

|  |  |  |
|---|---|---|
| **`abort_exceptions`  — UserError ** | mini-swe-agent | abort  (auth/permission/not_found/context_window/quota) |
| **`get_template_vars` inject host info to ai prompt** | mini-swe-agent | platform.uname()  system prompt |
| **4  skill source chain** | CLI-Anything _resolve_matrix_content_source | checkout → bundled → published URL → stub |
| **Tool schema  OpenAI  JSON schema** | mini-swe-agent BASH_TOOL | ,  |
| **Single tool `shell` ** | mini-swe-agent  BASH tool | ,  |
| **stable prefix convention** | CLI-Anything HARNESS_PREFIX | `AI_BUILTIN_PREFIX = "ai."`  |

### 3.4 P3 / ( API)

|  |  |  |
|---|---|---|
| **`_find_repo_root` git + parent walk** | CLI-Anything | Mora  |
| **`KIND_LABELS` UI short names** | CLI-Anything | Mora  |
| **multi state file separation** | CLI-Anything | Mora ,  |
| **FormatError MUST  response** | mini-swe-agent | Mora record  spec contract |
| **TTL cache + timestamp ** | CLI-Anything | Mora  |
| **multi package manager abstraction** | CLI-Anything | Mora  |
| **`shutil.ignore_patterns` clean reinstall** | CLI-Anything | Mora  |

---

## 4. 

### 4.1 

1. **v1 (7 AI ) ** ( module/builtin).   SmartCrusher ****.
2. **v2 (mini-swe-agent + CLI-Anything) ** ().   exceptions-as-flow ****,  module.
3. **v0.30-0.33  5  module  Interpreter** —— 0 .  P0 ****, .

### 4.2 v0.34  ( v0.31 panic-refactor )

1. ** 5  v0.30-0.33 module  Interpreter** (1 )
2. ** Interrupt 5 ** (3 )
3. ** limits ** (1 )
4. ** abort_exceptions ** (1 )
5. ** Registry  + 3  fallback** (3 )
6. ** sandbox 3-mode** (3 )
7. ** shell.run ** (2 )

### 4.3  v1 (AGENTS_PRIMITIVES.md) 

|  |  |  |  |
|---|---|---|---|
| v1 (7 AI ) | 21  | **** () | `react` / `plan` / `document.grouped_layout` / `sandbox` |
| v2 (2 AI ) | 14  | **** () | `interrupt` / `limits` / `sandbox.run(3-mode)` / `registry cache` |

**v0.34+  = v1  4-5  + v2  14 **

### 4.4 v0.34 

|  |  |  |
|---|---|---|
| Integrate 5 modules (event/sandbox/ccr/schedule/mock) | 3d | v2 P0 |
| Add Interrupt 5 exception types | 2d | v2 1.1 |
| Add limits framework | 1d | v2 1.3 |
| Add abort_exceptions classification | 0.5d | v2 1.4 |
| Add Registry cache (3-layer fallback) | 2d | v2 2.1 |
| Add sandbox 3-mode | 2d | v2 1.2 |
| Add shell.run with process group kill | 1d | v2 1.6 |
| Add `COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` sentinel | 0.5d | v2 1.5 |
| **Total** | **~12d** | (v0.32-0.33 ,  sprint) |

### 4.5 

- v0.35: `react` + `plan` (v1 ) + `tool kind registry` (v2 2.8)
- v0.36: `document.grouped_layout` + `document.reading_order` (v1 , MinerU )
- v0.37: `mora serve --openai` (v1 , OpenInfer )
- v0.38+: schedule heartbeat / lifecycle / policy ( 7  v1 P2 )

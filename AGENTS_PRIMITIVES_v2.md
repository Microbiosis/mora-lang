# Mora v0.34+ 原语路线图 v2 — mini-swe-agent + CLI-Anything 深度源码分析

> **目的**: 通过 deep-dive 2 个 AI 工具项目 (mini-swe-agent, CLI-Anything) 的源码,
> 提取**实现原理**而非 README, 映射到 Mora 语言原语.
>
> **方法**: 实际 clone 仓库, 读 .py 源码 (非 README), 识别**模式**和**可迁移的设计**
>
> **对比 AGENTS_PRIMITIVES.md (v1)**: v1 是从 7 个 AI 基础设施 (AIOS/MimiClaw/
> OpenFugu/OpenInfer/MinerU/Headroom/Puter) 提取**功能/原语**. v2 是从 2 个**完整
> AI 工具**提取**模式/设计**.

---

## 0. 核心认知 (One-liner)

> v1 提的缺"新功能" (mora 已有 AI/压缩/文档能力). v2 提的缺"**模式**":
> exceptions-as-flow、3-mode 交互、multi-layer source fallback、TTL cache
> fallback、abort_exceptions 分类、interrupt taxonomy. Mora 缺的**不是新工具**,
> 是**让现有工具组合得更鲁棒的模式**.

---

## 1. mini-swe-agent 关键架构模式

`https://github.com/SWE-agent/mini-swe-agent` —— 3 个核心模块:
- `agents/default.py` (188 行): step/query loop
- `environments/local.py` (92 行): bash executor
- `models/litellm_model.py` (163 行): LLM 抽象

### 1.1 exceptions-as-flow 模式

**位置**: `agents/default.py:100-117` + `exceptions.py`

**模式**: 异常 = messages 流中的 `role: "exit"` 消息, 而非 Python 异常向上抛。

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
    raise  # fatal, 仍 raise
```

**5 种结构化 interrupt taxonomy** (`exceptions.py`):
- `FormatError` — LLM 输出格式错
- `InterruptAgentFlow` — 用户中断 (可继续)
- `LimitsExceeded` — step/cost 超限
- `TimeExceeded` — wall time 超限
- `Submitted` — 任务完成 (sentinel string 触发)
- `UserInterruption` — 来自 interactive mode

**Mora 现状**: Mora 的 `Result<Value, String>` 错误流是**直接抛字符串**，没有结构化 interrupt taxonomy。`src/interpreter/mod.rs:556-591` 的 `run_repl_with` 用 `?` 传递错误，**没有"错误 → messages 流"的概念**。

**Mora 原语 (P0)**:

```mora
// v0.34: Interrupt primitive (5 种结构化类型)
interrupt FormatError { message: String, response: Value }
interrupt LimitsExceeded { kind: String, current: number, limit: number }
interrupt TimeExceeded { elapsed_s: number, limit_s: number }
interrupt Submitted { output: String }
interrupt UserInterruption { kind: String, comment: String }

// builtin: emit interrupt 注入 messages 流
bus.emit("interrupt." + name, payload)
```

### 1.2 3-mode 交互 (human/confirm/yolo)

**位置**: `agents/interactive.py:25-29, 165-182`

**模式**: 同一 agent, 3 种**用户介入级别**:
- `human`: 用户的命令直接执行 (不调 LM)
- `confirm`: LM 的命令需用户确认 (`y` 确认 / `/u` 切到 human)
- `yolo`: LM 的命令直接执行 (无确认, CI 用)

**Whitelist 机制**: `whitelist_actions: list[str]` 正则匹配免确认（`rm -rf /` 不在白名单 → 必确认）。

```python
# interactive.py:162-163
def _should_ask_confirmation(self, action: str) -> bool:
    return self.config.mode == "confirm" and not any(
        re.match(r, action) for r in self.config.whitelist_actions
    )
```

**CI-safe stdin check** (interactive.py:97-107): `sys.stdin.isatty()` 检查, CI 环境（`stdin = /dev/null`）不弹 prompt 避免 `EOFError`。

**Mora 现状**: Mora v0.33 加了 `SandboxPolicy` 有 `allow/deny`，**但无 user interaction**。Script 不能"问用户"。

**Mora 原语 (P1)**:

```mora
// v0.34: sandbox run mode (3-mode like interactive.py)
let result = sandbox.run(script, {
    mode: "confirm",         // "human" | "confirm" | "yolo"
    whitelist: ["^ls", "^cat"],  // 正则匹配免确认
    stdin_tty_check: true,    // CI 安全 (stdin=/dev/null 不弹 prompt)
})

// builtin: interrupt for user confirmation
if interrupt? then
    let choice = interrupt.comment  // "y" | "/u" | user comment
    handle_user_decision(choice)
end
```

### 1.3 三种 limits 统一框架

**位置**: `agents/default.py:130-145` + `AgentConfig` (default.py:19-35)

```python
# AgentConfig
step_limit: int = 0           # 0 = no limit
cost_limit: float = 3.0       # 总成本
wall_time_limit_seconds: int = 0

# query() 检查
if 0 < self.config.step_limit <= self.n_calls or
   0 < self.config.cost_limit <= self.cost:
    raise LimitsExceeded(...)
if 0 < self.config.wall_time_limit_seconds <= int(time.time() - self._start_time):
    raise TimeExceeded(...)
```

**关键模式**: 三种 limits **独立检查**, 触发不同 exception, **不合并**。`step_limit == 0` 表示无限制（**default to permissive**）。

**Mora 现状**: Mora `ai_infra::TokenBudget` 有 `step_limit` 概念但**实际未用**（`#[allow(dead_code)]`）。Mora 没有 cost_limit / wall_time_limit 框架。

**Mora 原语 (P0)**:

```mora
// v0.34: 统一 limits block
ai.limits({
    step: 100,              // 最多 100 步
    cost: 3.0,              // 最多 $3
    wall_time_s: 600,       // 最多 10 分钟
}) {
    let answer = ai.chat(p"...")
    // 自动检查: 超 step 抛 LimitsExceeded(step)
    //          超 cost 抛 LimitsExceeded(cost)
    //          超 wall_time 抛 TimeExceeded
}
```

### 1.4 abort_exceptions 分类 (retry 关键)

**位置**: `models/litellm_model.py:50-57` + `models/utils/retry.py`

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

**模式**: `retry_if_not_exception_type(tuple(abort_exceptions))` —— **abort 类型不重试**。`UnsupportedParamsError` / `NotFoundError` / `PermissionDeniedError` / `ContextWindowExceededError` / `AuthenticationError` / `KeyboardInterrupt` 都是**用户错误或不可恢复**，重试无意义。

**Mora 现状**: Mora `src/interpreter/mod.rs:73-99` `is_retryable_error()` 启发式判断（network/429/5xx 重试），但**没列 abort_exceptions**。

**Mora 原语 (P2)**:

```rust
// src/interpreter/ai_chat.rs
const ABORT_EXCEPTIONS: &[&str] = &[
    "auth", "permission", "not_found", "context_window", "quota",
];
// retry 前先 check
if error_msg.contains_any(ABORT_EXCEPTIONS) {
    return Err(error);  // 不重试
}
```

### 1.5 sentinel string submit

**位置**: `environments/local.py:45-56`

```python
def _check_finished(self, output: dict):
    lines = output.get("output", "").lstrip().splitlines(keepends=True)
    if lines and lines[0].strip() == "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT" and output["returncode"] == 0:
        submission = "".join(lines[1:])
        raise Submitted(...)
```

**模式**: bash 输出**第一行** 是 sentinel string 触发提交。**简单协议**——比 `is_done()` 函数更易跨语言。**output 后续行** 是 submission。

**Mora 现状**: Mora `mcp_server` 工具调用是 `Value::Dict` 返回，没用 sentinel string。**问题**: 工具返回多类型时 (`Dict`/`List`/`String`)，需要"完成"语义。

**Mora 原语 (P1)**:

```mora
// v0.34: mcp tool 提交 sentinel
tool.shell("run_tests")  // 返回 "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT\nall 12 tests passed"
// builtin 解析: 第一行 = sentinel, 后续 = submission
mcp.submit(submission)  // 触发 Submitted interrupt
```

### 1.6 进程组 kill 防孤儿

**位置**: `environments/local.py:84, 89`

```python
process = subprocess.Popen(
    command, shell=True, text=True, cwd=cwd, env=env, ...,
    start_new_session=os.name == "posix",  # 创建新 session (process group)
)
try:
    stdout, _ = process.communicate(timeout=timeout)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL) if os.name == "posix" else process.kill()
    stdout, _ = process.communicate()
    raise subprocess.TimeoutExpired(command, timeout, output=stdout)
```

**模式**: `start_new_session=True` (POSIX) 创建 process group, timeout 时 `os.killpg` 杀**整组**而非单进程, 避免孤儿子进程。

**Mora 现状**: Mora 无 `shell` builtin（v0.20 删了相关）。但 v0.33 sandbox policy 有 path validation 缺 process group kill。

**Mora 原语 (P2)**:

```mora
// v0.34: shell.run 进程组隔离
let result = shell.run("make test", {
    timeout_s: 30,
    kill_process_group: true,  // POSIX 杀整组防孤儿
})
```

### 1.7 FormatError MUST 持久化 response (spec contract)

**位置**: `models/litellm_model.py:88-97`

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

**模式**: 即使 parse 失败, response **必须** 持久化。Spec contract — 让 trajectory 完整, 调试时可看到 LLM 实际返回什么。

**Mora 现状**: Mora `record` 模块做 AI call 录制（`src/record/`），但**不保证** parse 失败的也录。

**Mora 原语 (P2)**:

```mora
// v0.34: record 包 parse 失败也持久化
record.config({on_error: "persist"})
let response = ai.chat(p"...")  // 即使 JSON parse 失败, response 也录
```

### 1.8 OpenAI 标准 tool schema (单 tool `bash`)

**位置**: `models/utils/actions_toolcall.py:8-23`

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

**模式**: **单 tool `bash`**, 不用 Read/Edit/Glob 多种 tool——**避免工具爆炸**, 让 LLM 自由组合命令。

**Mora 现状**: Mora `tool_def` 是单 tool, **但** `mcp_server` 注册工具是 OpenAI 标准 JSON schema。**已对齐**。

**Mora 原语 (P3)**: 已经实现, 无需新原语.

### 1.9 Pydantic config + `model_dump(mode="json")` 序列化

**位置**: 整个 codebase, 例 `LitellmModelConfig(BaseModel)` (litellm_model.py:27)

**模式**: Pydantic BaseModel + `model_dump(mode="json")` 序列化保证 JSON-safe。

**Mora 现状**: Mora 不用 Pydantic (无 Python)。`Serialize` 用 `serde::Serialize` 类似思想（Rust 自带），但 **Mora 没**——value.rs 没 derive Serialize。

**Mora 原语 (P3)**: 给 Value enum 加 `Serialize` + `Deserialize` derive. 但 v0.31 明确"0 新外部依赖"，`serde_json` 是 transitive (经 `undoc`) 但**没 derive**。可加 `serde::Serialize` derive for Value（已有 dependency）。

---

## 2. CLI-Anything 关键架构模式

`https://github.com/HKUDS/CLI-Anything` —— 大量子目录（每个 GUI 工具对应 skill）。
核心在 `cli-hub/cli_hub/` (4931 行 8 文件):

- `registry.py` (117): 双 registry 拉取 + TTL cache
- `matrix.py` (537): matrix 数据
- `matrix_skill.py` (397): 4 层 source fallback 渲染 SKILL.md
- `installer.py` (604): npm/uv 安装
- `cli.py` (1030): CLI 命令

### 2.1 双 registry + 3 层 cache fallback

**位置**: `registry.py:32-90` + `matrix.py:48-80`

**模式**: network → cache → local file (3 层 fallback):

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
            return cached_data  # ← 用 stale cache 而非崩
        raise
    cache_payload = {"_cached_at": time.time(), "data": data}
    cache_file.write_text(json.dumps(cache_payload, indent=2))
    return data
```

**关键**: 2nd fallback (stale cache on network error) **永远 raise 之前**。

**Mora 现状**: Mora `mcp_server` 无 registry 缓存。`mcp_server.rs` 加载工具直接 hardcode 列表。

**Mora 原语 (P1)**:

```rust
// v0.34: Registry 缓存 + fallback
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

### 2.2 multi-layer source fallback (4 层)

**位置**: `matrix_skill.py:152-171` `_resolve_matrix_content_source`

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

**模式**: 4 层 source chain —— checkout → bundled → published URL → generated stub。每个 fallback **独立可降级**，last 总是 stub（永不 raise）。

**Mora 现状**: Mora `document` backend **单源**（每 backend hardcode path）。`mcp_server` 工具列表 hardcode。

**Mora 原语 (P2)**:

```mora
// v0.34: skill loader 4 层 source
let skill = skill.load("./skills/greet.md", {
    sources: [
        skill.source_checkout,    // ./skills/greet.md
        skill.source_bundled,     // ~/.mora/skills/greet.md
        skill.source_published,   // https://...
        skill.source_stub,        // 生成的 placeholder
    ]
})
```

### 2.3 `_find_repo_root` git + parent walk

**位置**: `matrix_skill.py:45-65`

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

**模式**: git first (有 git 时), then 父目录 walk (无 git 也 work)。**双策略**互补。

**Mora 现状**: Mora `mcp_server` 工具加载 hardcode path。**缺** dev/installed 双模式检测。

**Mora 原语 (P3)**:

```rust
// v0.34: dev 模式自动检测
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

### 2.4 stable prefix constant (集中命名约定)

**位置**: `matrix.py:25`

```python
HARNESS_PREFIX = "cli-anything-"  # 集中管理 harness CLI 命名约定
```

**配套**: `_provider_installed` (matrix_skill.py:270) 用 `aliases = {name, name.removeprefix(HARNESS_PREFIX)}` 处理两种命名。

**Mora 现状**: Mora `ai.*` builtin 散在 5 个文件 (`ai_chat.rs`, `builtins.rs`, `ai_helpers.rs`, `orchestrate.rs`, `main.rs`)。**没有**集中命名约定。

**Mora 原语 (P3)**:

```rust
// v0.34: src/builtins_prefix.rs 集中命名
pub const AI_BUILTIN_PREFIX: &str = "ai.";
pub const MEMORY_BUILTIN_PREFIX: &str = "memory.";
pub const FILE_BUILTIN_PREFIX: &str = "file.";
// builtin 命名一致性: rust 函数命名 + builtin 字符串统一从这来
```

### 2.5 stable short labels (UI 友好)

**位置**: `matrix.py:31-42` `KIND_LABELS`

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

**模式**: 内部名 (`harness-cli`) → UI 名 (`harness`)，**1-1 映射 dict**。

**Mora 现状**: Mora `Value::Display` 是 ad-hoc per-variant (`<http_request POST /a>`)。**缺**统一 short-label map。

**Mora 原语 (P3)**:

```rust
// v0.34: 集中 short label
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

**位置**: `installer.py:13-15` (两个 `Path` 常量)

```python
INSTALLED_FILE = Path.home() / ".cli-hub" / "installed.json"
MATRIX_STATE_FILE = Path.home() / ".cli-hub" / "matrix_state.json"
```

**模式**: **不同关注点**用不同 state file: `installed.json` (installed CLIs), `matrix_state.json` (matrix 安装状态)。

**Mora 现状**: Mora `record/` 模块有 `recorder.jsonl` (JSONL 单文件)。`~/.mora_schedule.json` (v0.33 schedule) 单文件。**不同 builtin 用不同 state file** 已经隐含实现。

**Mora 原语 (P3)**: 已经实现 pattern, 无需新原语. 文档化即可.

### 2.7 `_copy_matrix_assets` 整体替换 + ignore_patterns

**位置**: `matrix_skill.py:174-190`

```python
def _copy_matrix_assets(content_dir, output_dir):
    copied = []
    for subdir in MATRIX_ASSET_SUBDIRS:  # ("references", "scripts")
        source = content_dir / subdir
        destination = output_dir / subdir
        if destination.exists():
            shutil.rmtree(destination)  # ← 先清后拷
        if source.is_dir():
            shutil.copytree(source, destination, ignore=_COPY_IGNORE)
            copied.append(subdir)
    return copied
```

**模式**: 整体替换 + ignore pattern (排除 `__pycache__`, `*.pyc`, `*.pyo`)。**idempotent re-install**。

**Mora 现状**: Mora `document` backend 加载 docx 时 resources 不在同目录, 临时解。**MCP tool install** 无。

**Mora 原语 (P3)**:

```rust
// v0.34: mcp tool install — clean reinstall
fn install_tool(name: &str, force: bool) -> Result<PathBuf, String> {
    let dest = tools_dir().join(name);
    if dest.exists() && force {
        std::fs::remove_dir_all(&dest)?;  // 整体替换
    }
    // copy from source, excluding __pycache__...
}
```

### 2.8 kind registry pattern (3-way filter)

**位置**: `matrix.py:23-25, 537+`

```python
AGENT_INSTALLABLE_KINDS = {"agent-skill"}
INSTALLABLE_KINDS = {"harness-cli", "public-cli"}

# 多处用 `if kind in AGENT_INSTALLABLE_KINDS` 过滤
def render_matrix_skill_file(matrix_item, installed=None):
    for cli in ...:
        if cli["kind"] not in {"harness-cli", "public-cli"}:
            return False  # 跳过非 CLI kind
```

**模式**: kind registry 用**多个 set** 表达**不同 filter 维度**。**同一字段 `kind` 不同视角下不同**。

**Mora 现状**: Mora `tool_def` 单 kind (`builtin`)。`mcp_server` 无 kind 概念。

**Mora 原语 (P2)**:

```rust
// v0.34: tool kind registry
pub const TOOL_KIND_BUILTIN: &str = "builtin";     // mora 内置
pub const TOOL_KIND_SHELL: &str = "shell";         // shell command
pub const TOOL_KIND_HTTP: &str = "http";           // HTTP API
pub const TOOL_KIND_SKILL: &str = "skill";         // markdown 教学
pub const TOOL_KIND_NATIVE: &str = "native";       // binary
```

### 2.9 TTL cache + timestamp 字段

**位置**: `registry.py:39, 54-55`

```python
if time.time() - cached.get("_cached_at", 0) < CACHE_TTL:
    return cached["data"]
# ...
cache_payload = {"_cached_at": time.time(), "data": data}
```

**模式**: 缓存 wrapper 用 **`_cached_at` 字段**, 不是文件 mtime。

**Mora 现状**: Mora 无 registry 缓存。

**Mora 原语 (P1)**: 见 2.1.

### 2.10 multi package manager abstraction

**位置**: `installer.py:53-64`

```python
def _find_npm():
    return shutil.which("npm")
def _find_uv():
    return shutil.which("uv")
```

**模式**: `shutil.which` 检测 package manager binary 是否在 PATH, 返回路径或 None。

**Mora 现状**: Mora 无 package manager 抽象 (Mora 脚本**本身**就是用户写的)。

**Mora 原语 (P3)**:

```mora
// v0.34: package manager 检测 builtin (开发者用)
let npm = shell.which("npm")   // 找 npm 在 PATH 里的路径
let uv = shell.which("uv")
// 返回 nil if not found
```

---

## 3. Mora v0.34+ 原语路线图 v2

> v1 (AGENTS_PRIMITIVES.md) 从 7 个 AI 基础设施提 21 个新原语 (功能).
> v2 (本文) 从 2 个完整 AI 工具提 **模式** (process), **不增加新原语数量**但**让现有原语组合更鲁棒**.

### 3.1 P0 必修 (直接复用 v0.30-0.33 已实现的 module)

| 原语 | 灵感 | 文件 | 关键设计 |
|---|---|---|---|
| **Integrate `event::EventBus` as builtin** | mini-swe-agent exception-as-flow | `src/interpreter/builtins.rs` | `bus.emit(name, payload)` builtin 注册 |
| **Integrate `sandbox::SandboxPolicy` as builtin** | mini-swe-agent whitelist | `src/interpreter/builtins.rs` | `sandbox.run(script, {allow, deny})` builtin |
| **Integrate `ccr::CcrStore` as builtin** | Headroom CCR (已有 v0.33) | `src/interpreter/builtins.rs` | `ccr.put(data) -> hash`, `ccr.get(hash) -> data` |
| **Integrate `schedule::Scheduler` as builtin** | MimiClaw cron (已有 v0.33) | `src/interpreter/builtins.rs` | `schedule.add(name, kind, msg, interval) -> id` |
| **`ai.limits({step, cost, wall_time})` block** | mini-swe-agent AgentConfig | `src/interpreter/ai_chat.rs` | 3 种 limit 独立检查, interrupt 注入 messages |
| **Interrupt primitive 5 种** | mini-swe-agent exception-as-flow | `src/interpreter/mod.rs` | `interrupt FormatError/LimitsExceeded/TimeExceeded/Submitted/UserInterruption` |
| **`shell.run` 进程组隔离** | mini-swe-agent killpg | `src/interpreter/builtins.rs` | `shell.run(cmd, {killpg: true, timeout_s: 30})` |
| **Registry 缓存 + 3 层 fallback** | CLI-Anything _fetch_json | `src/mcp_server.rs` (新 module `src/registry.rs`) | network → stale cache → local file |

### 3.2 P1 应做 (新功能)

| 原语 | 灵感 | 设计 |
|---|---|---|
| **`sandbox.run({mode: "human"|"confirm"|"yolo"})`** | mini-swe-agent 3-mode | 用户脚本可弹 prompt 确认危险操作 |
| **`COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` sentinel** | mini-swe-agent _check_finished | mcp tool 输出第一行 = sentinel → 触发 Submit interrupt |
| **ToolError 统一格式** | CLI-Anything _format_requires | `{kind, requires, notes, status}` 统一 schema |
| **`mcp.tool.list()` returns TTL-cached** | CLI-Anything registry | 工具列表缓存 + 网络降级 |
| **`mcp.tool.install(name)` with local checkout fallback** | CLI-Anything matrix install | 4 层 source: checkout → bundled → published → stub |
| **`retry` decorator builtin** | mini-swe-agent tenacity | `retry.with({abort: [...], max: 10, backoff: "exponential"}) { ... }` |
| **InterruptAgentFlow 异常族** | mini-swe-agent exceptions | 5 种结构化 interrupt taxonomy |

### 3.3 P2 长期 (增强现有)

| 原语 | 灵感 | 设计 |
|---|---|---|
| **`abort_exceptions` 列表 — UserError 不重试** | mini-swe-agent | abort 列表 (auth/permission/not_found/context_window/quota) |
| **`get_template_vars` inject host info to ai prompt** | mini-swe-agent | platform.uname() 注入 system prompt |
| **4 层 skill source chain** | CLI-Anything _resolve_matrix_content_source | checkout → bundled → published URL → stub |
| **Tool schema 用 OpenAI 标准 JSON schema** | mini-swe-agent BASH_TOOL | 已经实现, 无需新原语 |
| **Single tool `shell` 避免工具爆炸** | mini-swe-agent 单 BASH tool | 已经实现, 无需新原语 |
| **stable prefix convention** | CLI-Anything HARNESS_PREFIX | `AI_BUILTIN_PREFIX = "ai."` 集中管理 |

### 3.4 P3 文档/重构 (不改 API)

| 任务 | 灵感 | 现状 |
|---|---|---|
| **`_find_repo_root` git + parent walk** | CLI-Anything | Mora 缺 |
| **`KIND_LABELS` UI short names** | CLI-Anything | Mora 缺 |
| **multi state file separation** | CLI-Anything | Mora 已隐含实现, 需文档化 |
| **FormatError MUST 持久化 response** | mini-swe-agent | Mora record 缺 spec contract |
| **TTL cache + timestamp 字段** | CLI-Anything | Mora 缺 |
| **multi package manager abstraction** | CLI-Anything | Mora 缺 |
| **`shutil.ignore_patterns` clean reinstall** | CLI-Anything | Mora 缺 |

---

## 4. 总结

### 4.1 关键认知

1. **v1 (7 AI 基础设施) 提的是「功能原语」** (新 module/builtin).  例如 SmartCrusher 是**新功能**.
2. **v2 (mini-swe-agent + CLI-Anything) 提的是「模式」** (让现有功能更鲁棒).  例如 exceptions-as-flow 是**新模式**, 不需新 module.
3. **v0.30-0.33 加的 5 个 module 都未整合进 Interpreter** —— 0 引用. 真正的 P0 是**集成**, 不是新原语.

### 4.2 v0.34 优先做 (按 v0.31 panic-refactor 模式)

1. **集成 5 个 v0.30-0.33 module 进 Interpreter** (1 周)
2. **加 Interrupt 5 种异常族** (3 天)
3. **加 limits 统一框架** (1 天)
4. **加 abort_exceptions 分类** (1 天)
5. **加 Registry 缓存 + 3 层 fallback** (3 天)
6. **加 sandbox 3-mode** (3 天)
7. **加 shell.run 进程组隔离** (2 天)

### 4.3 与 v1 (AGENTS_PRIMITIVES.md) 关系

| 版本 | 数量 | 关注点 | 例子 |
|---|---|---|---|
| v1 (7 AI 基础设施) | 21 个 | **功能** (新原语) | `react` / `plan` / `document.grouped_layout` / `sandbox` |
| v2 (2 AI 工具) | 14 个 | **模式** (新使用方式) | `interrupt` / `limits` / `sandbox.run(3-mode)` / `registry cache` |

**v0.34+ 路线图 = v1 选 4-5 个未完成原语 + v2 全部 14 个模式**。

### 4.4 真要做的（v0.34 实际工作量）

| 任务 | 工作量 | 灵感来源 |
|---|---|---|
| Integrate 5 modules (event/sandbox/ccr/schedule/mock) | 3d | v2 P0 |
| Add Interrupt 5 exception types | 2d | v2 1.1 |
| Add limits framework | 1d | v2 1.3 |
| Add abort_exceptions classification | 0.5d | v2 1.4 |
| Add Registry cache (3-layer fallback) | 2d | v2 2.1 |
| Add sandbox 3-mode | 2d | v2 1.2 |
| Add shell.run with process group kill | 1d | v2 1.6 |
| Add `COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` sentinel | 0.5d | v2 1.5 |
| **Total** | **~12d** | (v0.32-0.33 模式, 一个 sprint) |

### 4.5 给后续版本

- v0.35: `react` + `plan` (v1 选) + `tool kind registry` (v2 2.8)
- v0.36: `document.grouped_layout` + `document.reading_order` (v1 选, MinerU 灵感)
- v0.37: `mora serve --openai` (v1 选, OpenInfer 灵感)
- v0.38+: schedule heartbeat / lifecycle / policy (剩余 7 个 v1 P2 原语)

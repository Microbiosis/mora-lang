# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)
[![CI](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml)

**Mora** — an AI-native statically-typed scripting language for LLM
agent orchestration and cloud-native observability. One binary ships both
an HTTP server and an MCP server.

[Architecture](docs/ARCHITECTURE.md) · [CHANGELOG](CHANGELOG.md) · [Influences](docs/influences.md) · [Spec](docs/mora-spec.md)

## Highlights

- **AI-native primitives** — first-class prompt literals `p"..."`,
  `ai.chat` / `ai.embed` / `ai.cosine` / `ai.search`, `route fast:` /
  `route deep:`, `agent.create().run()`, `try ... catch e: AiError`,
  SHA-256 audit-log hash chain.
- **HTTP + MCP in one binary** — `mora script.mora` exposes both a REST
  server (`serve as http on port N`) and an MCP server (`serve as mcp`)
  from the same script; CLI subcommand `mcp tool-list` enumerates tools.
- **Pregel BSP for multi-agent orchestration** — `state` / `node` /
  `channel` / `checkpoint` / `command` / `send` / `interrupt` / `rewind`
  primitives (v0.50); `orchestrate moa` (parallel LLM aggregation,
  v0.75.84) and `orchestrate moe` (sparse top-k experts, v0.75.85)
  built on top.
- **Record / replay / diff** — `mora record` / `replay` / `diff` for
  deterministic AI-call regression testing, `.mora/recordings/*.jsonl`.
- **Single-pass compile + MIR SSA + JIT** — Lexer → ParserV3 →
  `MirFunction<MirInst>` + `MirWitness` → witness typecheck → MIR
  optimize → DAG → `vm::run_mir`; copy-and-patch JIT always compiled
  (v0.75.43, no feature gate).
- **HM type inference** — Hindley-Milner inference opt-in via
  `MORA_HM=1`; default witness-based typecheck on by default
  (`MORA_NO_TYPECK=1` to skip).
- **LSP server (`mora-lsp`)** — hover / completion / definition /
  references / rename / semanticTokens / foldingRange /
  publishDiagnostics over stdio JSON-RPC 2.0.
- **9-language heritage** — Clojure, Common Lisp, Prolog, Lisp,
  Ballerina, StreamIt, APL, Logo, Smalltalk (see
  [docs/influences.md](docs/influences.md)).

## Install

Pre-built binaries at
[Releases](https://github.com/Microbiosis/mora-lang/releases/latest)
include both `mora` and `mora-lsp`.

| Target                  | Asset                                          |
|-------------------------|------------------------------------------------|
| Windows x86_64          | `mora-x86_64-pc-windows-msvc.zip`              |
| Linux x86_64 (glibc)    | `mora-x86_64-unknown-linux-gnu.tar.gz`         |
| Linux x86_64 (musl)     | `mora-x86_64-unknown-linux-musl.tar.gz`        |
| macOS Intel             | `mora-x86_64-apple-darwin.tar.gz`              |
| macOS Apple Silicon     | `mora-aarch64-apple-darwin.tar.gz`             |

Add the extracted binary to `PATH`; you get both `mora` and `mora-lsp`.

From source: `cargo build --release` produces the same binaries.

## Quick start

```bash
mora script.mora          # run a script
mora --repl               # interactive REPL
mora --check script.mora  # type check only
mora --opt=1 script.mora  # SSA optimization: 0=off / 1=basic / >=2=aggressive
mora-lsp                  # language server (stdio JSON-RPC)
```

## Example — HTTP + MCP + tracing in one script

```mora
-- Mora = HTTP + MCP + trace in one binary
observe trace

route fast: ai_model("gpt-4o-mini", temperature: 0.7)
route deep: ai_model("gpt-4o")

serve as http on port 3000 do
  GET "/health" -> fn(req)
    return {status: "ok", version: "v0.04"}
  end

  POST "/chat" -> fn(req)
    span "user_chat" tags {path: "/chat"} do
      let text = req["body"]["text"]
      let answer = deep(p"deep answer: {text}")
      record_tokens(120, answer.len())
      return answer
    end
  end
end

serve as mcp do
  tool search(query: string): string do
    return "found docs for: " + args["query"]
  end
end
```

Run with `mora script.mora` — both HTTP (3000) and MCP (stdio) come up.

## Language at a glance

| Concept            | Syntax                                                       |
|--------------------|--------------------------------------------------------------|
| Variables          | `let x = 1`, `let s: string = ""`                            |
| Tasks (named fns)  | `task foo(x: string): string ... end`                        |
| Anonymous fns      | `fn(x) return x + 1 end`                                     |
| Lists              | `[1, 2, 3]`, `list.map(fn)`, `list.filter(fn)`, `list.reduce(fn, init)` |
| Dicts              | `{key: val}`, `dict.get("key")`                              |
| String methods     | `.len()`, `.upper()`, `.lower()`, `.trim()`, `.split()`, `.contains()`, `.replace()` |
| Control flow       | `if/then/end`, `for x in list/end`, `match expr with/end`    |
| Pipe               | `data \|> func()`                                            |
| Parallel           | `parallel ... end`                                           |
| Modules            | `import "path"`, `export let/task`                           |
| Traits + impls     | `trait`, `impl ... for ...`, `dyn dispatch`, default impls   |
| Generics           | `Container<T>`, `where T: Comparable`                        |
| Types              | `type Name = TargetType`                                     |
| Enums              | `enum Name { V1, V2(T) }`                                    |
| Structs            | `struct Name { field: Type }`                                |
| References         | `&expr`, `&mut expr`, lifetime `'a`                          |
| Macros             | `macro name(params) ... end`                                 |

### Borrowed-from history (v0.16 → v0.24)

| Version | Feature | From | Example |
|---------|---------|------|---------|
| v0.16 | Pattern match guards | Prolog | `match n with x when x > 0 -> ... end` |
| v0.16 | List destructuring | Prolog | `let [head, ...tail] = [1, 2, 3]` |
| v0.17 | Pipe `\|>` | StreamIt | `5 \|> fn(x) return x * 2 end` |
| v0.17 | Sliding window | StreamIt | `[1,2,3,4,5].window(3)` |
| v0.17 | Array shape/reshape | APL | `.shape()`, `.flatten()`, `.transpose()`, `.reshape()` |
| v0.17 | Array broadcasting | APL | `[1,2,3] * 2` → `[2,4,6]` |
| v0.18 | Function compose | Clojure | `compose(f, g, h)` |
| v0.18 | Partial application | Lisp | `partial(add, 10)` |
| v0.19 | Atoms + swap/deref | Clojure | `atom(0)`, `swap()`, `deref()` |
| v0.19 | Worker pool | Ballerina | `parallel worker w1 ... end end` |
| v0.19 | Transaction with compensation | Ballerina | `transaction ... compensation ... end` |
| v0.20 | Reflection | Smalltalk | `type_of()`, `is_instance()`, `methods_of()` |
| v0.20 | Macros | Common Lisp | `macro name(params) ... end` |
| v0.21 | References | Rust | `&expr`, `&mut expr`, lifetime `'a` |
| v0.22 | First-class AI primitives | — | prompt literal + chat / stream / tool |
| v0.24 | Type alias / enum / struct | — | `type Name = T` / `enum Name { V(T) }` / `struct Name { f: T }` |

## Standard library

| Namespace | Functions |
|-----------|-----------|
| `json.*`  | `json.parse(text)`, `json.stringify(value)` |
| `web.*`   | `web.fetch(url)` (HTTP via `ureq`) |
| `file.*`  | `read_text` / `write_text` / `append_text` / `read_bytes` / `write_bytes`, `exists` / `is_file` / `is_dir` / `size` / `list` / `mkdir` / `remove` / `rename` / `copy` / `touch`, `cwd` / `chdir` / `home_dir` / `join` / `abs` / `basename` / `dirname` / `extname` |
| Persistence | `save "file.json", value` / `load "file.json", var` |
| Stream I/O | `read "a.txt" into x` / `write "a.txt", content` / `append "a.txt", content` |
| Memory    | `memory.store` / `recall` / `forget` / `clear` / `list` / `len` |
| Event     | `bus.emit(name, payload)`, `bus.count()` |
| Sandbox   | `sandbox.check_builtin`, `sandbox.check_path` |
| Schedule  | `schedule.add(name, schedule, payload, [interval_seconds])`, `schedule.count()` |
| CCR       | `ccr.put(content)` / `ccr.get(hash)` |
| Mock      | mock backend for testing |
| Skill     | dual registry (CLI-Anything SKILL.md pattern, v0.46) |
| ToolPlane | Core/Extension adapter (v0.45) |

## AI surface

| Group | Construct | Notes |
|-------|-----------|-------|
| Prompt | `p"hello {name}"` | first-class prompt literal with interpolation |
| Conversation | `let conv = ai.create("model"); conv.chat("...")` | |
| Streaming | `for token in ai.stream(p"...")` | mock in tests; SSE in v0.04.1 |
| Tool calling | `tool name(args): T do ... end` | registered automatically into MCP `tool-list` |
| Context | `with model = "..." / budget = N` | token budget tracking |
| Errors | `try ... catch e: AiError` | dict `{message, code, retryable, attempts, cause}` |
| Token accounting | `record_tokens(input, output)` | |
| Embeddings | `ai.embed(text \| list)`, `ai.cosine/dot/euclidean/norm`, `ai.search(query, corpus, k?)` | |
| Routing | `route fast: ai_model("gpt-4o-mini")` | `fast(p"...")` shortcut |
| Observability | `observe trace`, `observe metrics`, `observe otel endpoint "..."`, `span "..." tags {k:v} do ... end` | |
| Memory | `memory.store/recall/forget/...` | |
| Agent | `agent.create(name, config).run(task)`, `agent.critic(text)` / `agent.critic(text, ctx)` | |
| Multi-agent | `orchestrate moe` (v0.75.85 sparse top-k), `orchestrate moa` (v0.75.84 parallel aggregation) | declared form, sequential or BSP depending on kind |

## Environment variables

| Var | Meaning | Default |
|-----|---------|---------|
| `OPENAI_API_KEY`     | AI API key            | unset → mock mode |
| `MORA_AI_MODEL`      | Default AI model      | `gpt-4o-mini` |
| `MORA_AI_BASE_URL`   | AI base URL           | `https://api.openai.com/v1` |
| `MORA_EMBED_MODEL`   | Embedding model       | `text-embedding-3-small` |
| `MORA_NO_TYPECK`     | `1` skips type check  | unset → enabled |
| `MORA_HM`            | `1` enables HM inference | unset → witness typecheck only |

## Server modes

`serve` exposes an HTTP server, MCP server, REPL, or stdio echo from a
single script. Combine multiple modes in one script to expose them all.

| Mode | Syntax | Purpose |
|------|--------|---------|
| HTTP | `serve as http on port N` | REST API |
| MCP  | `serve as mcp`            | Claude Desktop / MCP clients |
| REPL | `serve as repl`           | interactive REPL |
| Stdio | `serve as stdio`          | stdio echo (v0.04.1+) |

**v0.11 four-port fallback**: HTTP server requests port N; on `EADDRINUSE`
falls back to N+1, N+2, N+3 in sequence with `SO_REUSEADDR` to survive
`TIME_WAIT`. Banner line: `[serve] requested port 3000 unavailable, using
3001 instead`.

## Routing & observability

```mora
-- route aliases
route fast: "gpt-4o-mini"
route deep: "gpt-4o"

let s = fast(p"summarize: {text}")
let a = deep(p"analyze: {question}")
```

```mora
-- observe + span + token accounting
observe trace
observe otel endpoint "http://otel-collector:4317"

span "user_request" tags {user_id: u.id} do
  let r = deep(p"...")
  record_tokens(input, output)
end
```

## Recording & replay

```bash
mora record script.mora my-recording         # → .mora/recordings/my-recording.jsonl
mora replay script.mora my-recording          # deterministic replay
mora diff a-recording b-recording             # compare two recordings
mora record list                              # list recordings
mora record stats my-recording                # stats
mora record timeline my-recording             # call timeline
mora record export my-recording --format md   # export JSONL / Markdown
mora record audit my-recording                # secret scan (default `.moraignore`)
mora record report my-recording --verify "…"  # evidence report
mora snapshot script.mora my-snapshot         # snapshot test
mora mcp tool-list                            # list MCP tools
mora mcp tool-search <query>                  # search MCP tools
mora mcp toolsets                             # list toolsets
```

## Type checking

```mora
let name: string = "mora"          -- OK
let age: number = "thirty"         -- typeck: string → number
task add(a: number, b: number): number
  return a + b
end
add(1, 2)                          -- OK
add(1)                             -- error: 2 args expected
add("x", 2)                        -- error: arg 1 must be number
```

Errors emit `Type error at line N: …`. Library / builtin tasks fall back
to `Any` only when no signature is declared. Set `MORA_NO_TYPECK=1` to
bypass typeck.

## LSP

`mora-lsp` (binary in every release) speaks JSON-RPC over stdio. Editors
should configure it as the LSP launch command. Capabilities advertised
in `initialize`:

- `textDocumentSync` (full sync)
- `hover` — types / signatures for variables and tasks
- `completion` — local symbols + keywords + tasks + builtins
- `definition` — go-to-definition
- `references` — symbol references
- `documentSymbol` — outline
- `documentFormatting` + `documentRangeFormatting`
- `rename`
- `semanticTokens`
- `foldingRange` — `if` / `for` / `task` blocks
- `publishDiagnostics` — typeck results

Smoke test: `cargo run --example lsp_smoke` spawns `mora-lsp`, opens a
buffer, and asserts typeck diagnostics / hover / completion.

## Editor support

See [editors/](editors/) for the six supported editors:

| Editor        | Folder                       | Format                       |
|---------------|------------------------------|------------------------------|
| VS Code       | [vscode/](editors/vscode/)   | VSIX, TextMate grammar in `package.json` |
| Neovim        | [neovim/](editors/neovim/)   | `lua/mora-lsp.lua`           |
| Helix         | [helix/](editors/helix/)     | `languages.toml`             |
| Sublime Text  | [sublime/](editors/sublime/) | `mora.sublime-settings`      |
| Vim           | [vim/](editors/vim/)         | `ftplugin/mora.vim`          |
| Emacs         | [emacs/](editors/emacs/)     | `mora-mode.el`               |

CI builds `mora` + `mora-lsp` for the six targets above via
`.github/workflows/release.yml` and uploads to GitHub Releases.

## Pipeline

```
.mora ──► lexer ──► tokens ──► parser_v3 ──► MirFunction<MirInst> + MirWitness
        ──► witness typecheck ──► MIR SSA optimize (--opt=1/2)
        ──► DAG (pregel worker pool) ──► value runtime
                ├──► ai.stream / chat / tool / route / budget
                ├──► memory.store / recall
                ├──► agent.create().run()
                ├──► web.fetch / json.* / file.*
                └──► HTTP / MCP / record / replay
                                ▼
                LSP (mora-lsp) ──► hover / completion / diagnostics
```

Key modules under `src/`:

- `lexer.rs` — tokens
- `parser_v3/` — Arena-based MIR expression builder (v0.55)
- `mir/` — SSA IR + DAG + copy-and-patch JIT (v0.75.43) + `witness` typeck
- `interpreter/` — value runtime, AI builtins (`ai_chat.rs` + `builtins/`)
- `typeck/` — HM inference (`MORA_HM=1`) + witness checker
- `lsp/` — JSON-RPC server + 9 providers
- `pregel/` — v0.50 BSP worker pool
- `checkpoint/` — v0.50 Memory + SQLite persistence (optional `checkpoint-sqlite` feature)
- `audit/` — v0.42.1 SHA-256 hash-chained JSONL audit log
- `toolplane/` + `ccr/` — v0.45 Core/Extension adapter
- `compress/` — v0.29 context compression + JSON crush
- `skill/` — v0.46 dual registry
- `mcp_server.rs` / `http_server.rs` — server entry points

## Examples

Run any file with `mora path/to/file.mora`:

- `mcp_server_demo.mora` — MCP tool registration + serve
- `compress_demo.mora` / `compress_smart_demo.mora` / `compact_demo.mora` — v0.29/v0.30 compress + SmartCrusher
- `hm_basic_demo.mora` — HM type inference walk-through (set `MORA_HM=1`)
- `integration_v0_34.mora` — bus / sandbox / schedule / ccr / mock builtin tour
- `jit_bench.rs` — `cargo run --release --example jit_bench` (JIT vs interpreter)
- `lsp_smoke.rs` / `lsp_v04_smoke.py` — LSP end-to-end
- `hm_inference_examples.md` — HM inference playbook

## Specs & design

- [docs/mora-spec.md](docs/mora-spec.md) — language spec, 20 chapters
- [docs/influences.md](docs/influences.md) — 9-language heritage
- [docs/learning-plan.md](docs/learning-plan.md) — 6-stage learning path
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — runtime architecture (v0.50 snapshot)
- [CHANGELOG.md](CHANGELOG.md) — full version history

## Tests

611 unit tests across the `mora` crate (`cargo test`, 2026-08-04).

Recent CHANGELOG highlights:

- **v0.75.85** (2026-08-04) — `orchestrate moe` (sparse top-k Mixture-of-Experts)
- **v0.75.84** (2026-08-04) — `orchestrate moa` (Mixture-of-Agents)
- **v0.75.43** — copy-and-patch JIT (no external deps, always compiled)
- **v0.50** — Pregel BSP + Checkpoint (Memory / SQLite)
- **v0.45** — ToolPlane Core/Extension adapter
- **v0.42** — Capability Token + SHA-256 hash-chained Audit Sink
- **v0.22** — first-class AI primitives (`p"..."` / `with` / `stream` / `tool`)

## License

BSD-3-Clause — see [LICENSE](LICENSE).
Copyright (c) 2026 Microbiosis.

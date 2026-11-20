# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)
[![CI](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml)

**Mora** — an **AI-native statically-typed language** designed for **LLM internals, compute graphs & operators, LLM inference/training, and dynamic resource orchestration**, combining **Go-style cloud-native concurrency** with first-class language support for **HTTP and MCP services**.

> **定位（v0.110.0）**：AI-native 静态类型语言，专为大模型底层、计算图与算子、大模型推理/训练与资源动态调控而设计，同时具备 Go 的云原生并发属性，原生提供对 HTTP 与 MCP 服务的语言级友好支持。

[Architecture](docs/architecture-diagram.md) · [CHANGELOG](CHANGELOG.md) · [Influences](docs/influences.md) · [Spec](docs/mora-spec.md)

## Highlights

- **AI-native numeric, tensor & compute foundation** — strict `Int` (`i64`, overflow-checked) vs `Float` (`f64`) literals with hardware low-bitwidth `DType` suffixes (`f16`, `bf16`, `fp8`, `fp8_e5m2`, `f32`, `f64`, `i4`..`i64`, `u8`..`u64`, `bool`), 64-byte aligned compact `Tensor<DType, Shape, Device>` with zero-copy strided multi-dim slicing (`t[0..4, :, -1]`, `t[..., None]`), native matrix/tensor contraction `@` and power `**`, higher-order reverse/forward AD (`grad`/`value_and_grad`/`jvp`/`no_grad`, HVP), `TensorComputeDag` 4-pass fusion (`FusedFlashAttention`, `FusedRmsNormLinear`, `FusedLinearAct`, `FusedElementwise`) with runtime-dispatched AVX2+FMA/NEON multi-threaded kernels & `DagPlanCache`/`DagWorkspace`, PagedAttention `KVPageAllocator` (`kv_allocator` with byte quotas, priority preemption & watermark callbacks) + 64-byte aligned slab `MemoryPool` (`memory_pool`), scalar/tensor math (`math.*`, `stats.*`, `linalg.*`, `random.*`), and modular std workspace crates (`std/math/bigint`, pure-Mora `std/net/http` with Router/Chunked/Keep-Alive/SSE, and `std/mcp` with HTTP+SSE/Stdio/`prompts`/`Struct::schema()`).
- **Single-pass Control SSA compiler, stackful VM & `mora build`** — Lexer → iterative Pratt `ParserV3::compile` → `MirWitness[]` + `ControlProgram` SSA IR (`verify_program`) → SSA optimization (`--opt=0/1/2` or `MORA_OPT`: CopyProp, GVN-CSE, LICM, DCE) → heap-framed stackful VM (`runtime::vm`) with `O(1)` tail-call optimization (100,000+ frames) and `mora build <file> -o <bin>` self-contained single-binary bundling.
- **Unified Span-tracked Hindley-Milner type system** — single constraint solver (`src/typeck/check_mir.rs` + `src/typeck/hm/`) delivering 5 AI-native static checks (`DType`, `Shape`, `Device`, `Resource`, `Effect`), nominal generic `struct` (with defaults & compile-time `@tag(...)`) / payload `enum`, source-level `trait`/`impl`/`dyn Trait`, default & named keyword arguments, rigid multi-character generic parameters (`<T, Dtype>`), ML Value Restriction, effect-row subsumption, covariant `result<T, E>` & `task_handle<T>`, invariant `channel<T>`, fail-closed `unknown`, and cross-module `import` verification.
- **Go-style cloud-native concurrency & Tokio runtime** — first-class `spawn`, typed invariant `channel<T>` (unbuffered rendezvous & bounded FIFO), fair multi-arm `select` (`case recv`/`send`/`timeout`/`default`), hierarchical cooperative cancellation `context` (`task_handle<T>`), cancellable `sleep`, multi-threaded `TaskScheduler`, non-blocking `AsyncIoReactor` (`tcp_listen` / `tcp_connect` / `tcp_listener` / `tcp_stream`), and `FsPolicy` / `CheckedPath` filesystem confinement with isolated per-runtime virtual `cwd`.
- **Algebraic effects, dynamic dispatch & homoiconic metaprogramming** — `perform` / `handle` with multi-shot delimited continuations (`resume`, `Value::Continuation`), source-level `trait` / `impl` & `dyn` trait dispatch (`as dyn`, default methods, supertraits), `HostObject` (`Value::External(Arc<dyn HostObject>)`) for opaque stateful host objects, chunked persistent `Value::List`, lexical `Environment` (`HashMap` + `Arc<Environment>` parent chain), atoms (`atom` / `swap` / `deref`), typed pattern matching with guards, and Lisp-heritage homoiconicity (`macro`, `quote`, `quasiquote`, `eval`).
- **LSP server (`mora-lsp`) & editor ecosystem** — stdio JSON-RPC 2.0 server with unified HM hover/completion (`infer_program_types`), bounded headers (`MAX_CONTENT_LENGTH`), shared iterative RFC 8259 JSON parser (`MAX_JSON_DEPTH = 256`), and integrations for VS Code, Neovim, Helix, Sublime Text, Vim, and Emacs.
- **9-language heritage** — Clojure, Common Lisp, Prolog, Lisp, Ballerina, StreamIt, APL, Logo, Smalltalk (see [docs/influences.md](docs/influences.md)).

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

From source (Rust stable toolchain):

```bash
cargo build --release     # builds target/release/mora and target/release/mora-lsp
```

## Quick start

```bash
mora script.mora          # run a script
mora run script.mora      # explicit run subcommand
mora build script.mora -o app  # compile into a self-contained single binary
mora --opt=1 script.mora  # SSA optimization: 0=off / 1=basic / >=2=aggressive
mora --check script.mora  # type check only
mora --repl               # interactive REPL
mora install <url>        # install a .mora script into vendor/
mora --version            # print version
mora --help               # print CLI help
mora-lsp                  # language server (stdio JSON-RPC)
```

## Language at a glance

```mora
-- Matrix contraction (@), closures, lists, and pattern matching
task linear_step(w, x)
  return w @ x
end

let weights = [[1.0, 2.0], [3.0, 4.0]]
let input = [0.5, 1.5]
let activated = linear_step(weights, input).map(fn(v) v ** 2.0 end)
print("activated =", activated)

let classify = fn(n)
  match n with
    x when x > 0 -> "positive"
    0 -> "zero"
    _ -> "negative"
  end
end

print(classify(5))
```

| Concept            | Syntax                                                       |
|--------------------|--------------------------------------------------------------|
| Variables          | `let x = 1`, `let lr: float = 1e-3`, `let s: string = ""`    |
| Numeric literals   | `42` (`int`), `3.14` / `1e-4` (`float`), width suffixes (`1.0f16`, `1.0bf16`, `1.0fp8`, `8i8`, `255u8`) |
| Operators          | `+`, `-`, `*`, `/`, `%`, `**` (power), `@` (dot / matmul), `!` / `not`, `and`, `or`, `\|>` |
| Tasks (named fns)  | `task foo(x) ... end`, `task add(a: int, b: int): int`       |
| Anonymous fns      | `fn(x) return x + 1 end`, `fn(x: int) x + 1 end`             |
| Lists              | `[1, 2, 3]`, `.map(fn)`, `.filter(fn)`, `.reduce(fn, init)`, `.shape()`, `.reshape(...)` |
| Dicts              | `{key: val}`, `dict.get("key")`                              |
| String methods     | `.len()`, `.upper()`, `.lower()`, `.trim()`, `.split()`, `.contains()`, `.replace()` |
| Control flow       | `if/then/else/end`, `for x in list/end`, `while cond/end`, `match expr with/end` |
| Concurrency        | `spawn f()`, `channel(cap)`, `select ... end`, `context()`, `sleep(ms)`, `tcp_listen`/`tcp_connect` |
| Parallel & STM     | `parallel ... end`, `worker w ... end`, `transaction ... end`, `atom(v)` |
| Tensors & Autograd | `Tensor::zeros([m, n])`, `t[0..2, :, None]`, `grad(f)`, `value_and_grad(f)`, `jvp(f, x, dx)`, `no_grad ... end` |
| Compute DAG & Pool | `trace_dag(fn, ...)`, `dag.compile()`, `dag.run(...)`, `kv_allocator(...)`, `memory_pool(max_bytes)` |
| Algebraic effects  | `perform Name(arg)`, `handle Name { ... } { ... resume(...) }` |
| Dyn dispatch & OO  | `trait` / `impl`, `expr as dyn Name`, `dyn Name<T>`          |
| Modules            | `import "path"`, `export let/task/struct/enum/trait/impl`, `std/net/http`, `std/mcp` |
| Types & ADTs       | `type Name = TargetType`, `enum Name ... end`, `struct Name ... end` (with defaults & `@tag(...)`) |
| Metaprogramming    | `macro name(params) ... end`, `quote(expr)`, `quasiquote`, `eval(src)` |

## Static Hindley-Milner type checking

```mora
let name: string = "mora"
let steps: int = 42
let scale: float = 0.5

task add_steps(a: int, b: int): int
  return a + b
end

let total = add_steps(steps, 8)
let inc = fn(x: int) x + 1 end
print(name, inc(total), scale)
```

Type checking runs automatically before execution and on `mora --check <file>` (emitting `Type error at line N: ...` on mismatches). Unsuffixed integers (`42`) infer as exact 64-bit `int`, floating-point literals (`0.5`, `1e-3`, `1.0f32`) infer as `float`, and generic closures follow ML Value Restriction.

## Cloud-native concurrency (Go-style channels & `select`)

```mora
let ch = channel(1)
let ctx = context()

let h = spawn fn()
  ch.send(42)
  return 100
end

let msg = select
  case v = ch.recv() => v + 1
  timeout(500) => -1
end

print("msg =", msg, "task =", h.join(), "cancelled =", ctx.is_cancelled())
```

## Algebraic effects & multi-shot continuations

```mora
let total = handle Flip {
  let a = perform Flip()
  let b = perform Flip()
  a + b
} {
  resume(1) + resume(10)
}

print(total)
```

## Standard library builtins

| Namespace   | Highlights |
|-------------|------------|
| `json.*`    | `json.parse(text)`, `json.stringify(value)` |
| `file.*`    | `read_text`, `write_text`, `append_text`, `read_bytes`, `write_bytes`, `exists`, `is_file`, `is_dir`, `size`, `list`, `mkdir`, `remove`, `rename`, `copy`, `touch`, `cwd`, `chdir`, `home_dir`, `join`, `abs`, `basename`, `dirname`, `extname` |
| `exec.*`    | `exec.run(...)`, `exec.parallel(...)` |
| `math.*`    | scalar math helpers (`abs`, `floor`, `ceil`, `round`, `sqrt`, `pow`, `exp`, `ln`, `log2`, `log10`, trigonometry, `is_nan`, `is_infinite`, `is_finite`, ...) |
| `stats.*`   | `sum`, `mean`, `median`, `variance`, `stddev`, `percentile`, `histogram`, `cov`, `corr`, ... |
| `linalg.*`  | `dot`, `norm`, `transpose`, `matmul`, `cross`, `outer`, `det`, `inv`, `solve`, `trace`, ... |
| `random.*`  | deterministic / ambient PRNG helpers (`seed`, `rand`, `rand_int`, `normal`, `choice`, `shuffle`, ...) |

Additional std workspace crates live under `std/` (including `std/math/bigint` / `mora-std-bigint`).

## LSP & editor support

`mora-lsp` speaks JSON-RPC 2.0 over stdio and provides diagnostics (`publishDiagnostics`), HM-backed hover and completion, go-to-definition, references, rename, document symbols, formatting, semantic tokens, and folding ranges.

Smoke check:

```bash
cargo run --release --example lsp_smoke -- ./target/release/mora-lsp
```

See [editors/](editors/) for the six supported editors:

| Editor        | Folder                       | Format                       |
|---------------|------------------------------|------------------------------|
| VS Code       | [vscode/](editors/vscode/)   | VSIX, TextMate grammar in `package.json` |
| Neovim        | [neovim/](editors/neovim/)   | `lua/mora-lsp.lua`           |
| Helix         | [helix/](editors/helix/)     | `languages.toml`             |
| Sublime Text  | [sublime/](editors/sublime/) | `mora.sublime-settings`      |
| Vim           | [vim/](editors/vim/)         | `ftplugin/mora.vim`          |
| Emacs         | [emacs/](editors/emacs/)     | `mora-mode.el`               |

## Architecture & modules

```
.mora ──► lexer ──► parser_v3::compile ──► MirWitness[] + ControlProgram SSA IR
        ──► typeck (unified Span-tracked HM solver) ──► mir::control optimize (--opt=0/1/2)
        ──► runtime::vm (stackful VM) + runtime::scheduler / runtime::reactor (Tokio)
                ├──► traits / algebraic effects / closures / delimited continuations
                ├──► spawn / channel<T> / select / context / sleep / AsyncTcpListener
                ├──► json.* / file.* (FsPolicy + CheckedPath) / exec.* / math.* / stats.* / linalg.* / random.*
                └──► Value::External(Arc<dyn HostObject>) host extensions
                                ▼
                LSP (mora-lsp) ──► stdio transport + JSON-RPC + HM-unified providers
```

Key modules under `src/`:

- `lexer.rs` — tokenizer (`Int`/`Float` precision, numeric underscores, scientific notation, low-bitwidth suffixes, `**`, `@`, `!`)
- `parser_v3/` — iterative Pratt expression parser and single-pass Control SSA witness compiler
- `mir/control/` — SSA IR (`ControlProgram`, `BasicBlock`, `ControlInst`, `ControlTerminator`), lowering, verifier, and optimizer (`OptLevel`)
- `typeck/` — unified Span-tracked Hindley-Milner solver (`src/typeck/check_mir.rs`, `src/typeck/hm/`) and cross-module `import` symbol verification
- `runtime/` — stackful VM (`vm/`), `CoreRuntime`, process-shared `EngineShared`, `TaskScheduler` / `MoraChannel` / `AsyncIoReactor` (`scheduler.rs`, `reactor.rs`), reverse/forward autograd (`autograd.rs`), `TensorComputeDag` & `DeviceBackend` (`compute_dag.rs`), PagedAttention `KVPageAllocator` (`kv_cache.rs`), built-in domain/method handlers (`builtins/`), and `FsPolicy` / `CheckedPath` confinement (`fs.rs`)
- `value/` & `value.rs` — `Value` (including `Scalar`, `Tensor`, `StructInstance`, `EnumVariant`, `DynTrait`, `Channel`, `TaskHandle`, `Context`, `TcpListener`, `TcpStream`, `Continuation`, `External(Arc<dyn HostObject>)`), 64-byte aligned packed `tensor.rs`, chunked persistent `list.rs`, lexical `Environment`, and `display.rs`
- `flow/` & `flow.rs` — shared iterative RFC 8259 JSON parser/serializer and overflow-checked numeric tower
- `lsp/` — bounded stdio transport, JSON-RPC router, and HM-unified LSP providers
- `cli/` — shared CLI compilation, single-binary bundler (`bundle.rs`), interactive REPL (`repl.rs`), and path helpers
- `std/` — modular standard libraries (`std/math/bigint` Rust crate, pure-Mora `std/net/http.mora` with SSE, and `std/mcp.mora`)
- `error.rs` — unified domain-typed `MoraError` / `MoraResult` with `Span` diagnostics

### Architecture evolution (Wave 1–4)

- **Wave 1 (Completed, v0.110.0)** — Decoupled application-layer DSL constructs from the compiler core; extracted `BigInt` into `std/math/bigint` (`mora-std-bigint`) and focused the core runtime on language primitives and 7 foundational modules.
- **Wave 2 (Completed, v0.110.0)** — Unified execution around the single-pass Control SSA IR & heap-framed stackful VM (`O(1)` tail-call optimization, multi-shot delimited continuations), Tokio `TaskScheduler` & `AsyncIoReactor`, Go-style `spawn`/`channel<T>`/`select`/`context`/`sleep`, first-class async TCP (`tcp_listen`/`tcp_connect`/`tcp_listener`/`tcp_stream`), `FsPolicy`/`CheckedPath` sandboxed I/O, `HostObject` extensibility, and HM-unified `mora-lsp`.
- **Wave 3 — Stage 3A / 3B / 3C (Completed)** — Stage 3A delivered strict `Int`/`Float` lexical precision, low-bitwidth suffixes (`f16`/`bf16`/`fp8`/`i4`..`i64`), `**`/`@`/`!`, and the single Span-tracked HM constraint solver. Stage 3B added first-class `Tensor<DType, Shape, Device>` with 64-byte aligned packed storage and 5 static checks (DType, Shape, Device, Resource, Effect). Stage 3C delivered nominal `struct`/`enum`/`@tag` reflection, source-level `trait`/`impl`/`dyn`, default & named keyword arguments, zero-copy multi-dim `TensorSlice`, and reverse-mode `grad`/`value_and_grad`/`no_grad`.
- **Wave 4 (Completed · Track 4-1 Deepened)** — Native `TensorComputeDag` (`trace_dag`) with 4-pass fusion (`FusedFlashAttention` tiled online-softmax FlashAttention-2, `FusedRmsNormLinear`, `FusedLinearAct`, `FusedElementwise`), hardware-SIMD (`AVX2+FMA`/`NEON`) multi-threaded cache-blocked `CpuDeviceBackend`, compile-time `DagMemoryPlan` physical scratch-buffer reuse & `DagPlanCache`, arbitrary-order AD (`grad(grad(f))`, `jvp(grad(f), x, v)` exact HVP, `trace_dag(grad(f))`), PagedAttention-style `KVPageAllocator` (`kv_allocator`), `mora build` single-binary compilation, and pure-Mora `std/net/http` (with SSE) & `std/mcp` standard libraries.

## Examples & tests

Run the demo script or test suites:

```bash
cargo run -- examples/hm_basic_demo.mora
cargo test --all-targets
cargo test -p mora-std-bigint
```

## License

BSD-3-Clause — see [LICENSE](LICENSE).
Copyright (c) 2026 Microbiosis.

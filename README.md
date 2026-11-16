# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)
[![CI](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml)

**Mora** — a statically-typed functional scripting language with Hindley-Milner type inference, algebraic effects, persistent data structures, a Control SSA VM scheduled on Tokio, and a built-in LSP server (`mora-lsp`).

[Architecture](docs/architecture-diagram.md) · [CHANGELOG](CHANGELOG.md) · [Influences](docs/influences.md) · [Spec](docs/mora-spec.md)

## Highlights

- **Single-pass Control SSA compiler** — Lexer (strict Int vs Float literals, width suffixes, `**`, `@`, `!`) → iterative Pratt `ParserV3::compile` → `MirWitness[]` + `ControlProgram` SSA IR → optional SSA optimization (`--opt=0/1/2` or `MORA_OPT`) → heap-framed stackful VM (`runtime::vm`).
- **Unified Span-tracked Hindley-Milner type checking** — single constraint solver (`src/typeck/check_mir.rs` + `src/typeck/hm/`) supporting rigid generic parameters, qualified let-polymorphism with ML Value Restriction, covariant `result<T, E>`, fail-closed `unknown`, effect-row subsumption, and module checks across `import` boundaries before execution.
- **Algebraic effects & delimited continuations** — `perform` / `handle` with multi-shot `resume` continuations (`Value::Continuation`).
- **Dynamic dispatch & host extensibility** — `dyn` dispatch (`as dyn` coercion, default trait methods, runtime trait objects) plus the Rust `HostObject` trait (`Value::External(Arc<dyn HostObject>)`) for opaque stateful host objects.
- **Tokio concurrency, channels & confined filesystem** — Go-style `spawn`, typed invariant `channel<T>`, fair `select` (`case recv`/`send`/`timeout`/`default`), hierarchical cooperative cancellation `context` (`task_handle<T>`), cancellable `sleep`, multi-threaded `TaskScheduler`, non-blocking `AsyncIoReactor`, and `FsPolicy` / `CheckedPath` confinement with isolated per-runtime virtual `cwd`.
- **Functional core & persistent lists** — chunked persistent `Value::List` (`value/list.rs`), lexical `HashMap` + `Arc<Environment>` parent-chain bindings (`value.rs`), `EngineShared` runtime services, atoms (`atom` / `swap` / `deref`), `compose` / `partial`, pattern matching with guards, and macros.
- **LSP server (`mora-lsp`)** — stdio JSON-RPC 2.0 server with unified HM hover/completion (`infer_program_types`), bounded headers (`MAX_CONTENT_LENGTH`), and shared iterative RFC 8259 JSON engine (`MAX_JSON_DEPTH = 256`).
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
-- Named tasks, closures, lists, and pattern matching
task double_all(xs)
  return xs.map(fn(x) x * 2 end)
end

let nums = [1, 2, 3, 4]
let doubled = double_all(nums)
print("doubled =", doubled)

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
| Variables          | `let x = 1`, `let s: string = ""`                            |
| Tasks (named fns)  | `task foo(x) ... end`, `task add(a: number, b: number): number` |
| Anonymous fns      | `fn(x) return x + 1 end`, `fn(x: number) x + 1 end`         |
| Lists              | `[1, 2, 3]`, `.map(fn)`, `.filter(fn)`, `.reduce(fn, init)` |
| Dicts              | `{key: val}`, `dict.get("key")`                              |
| String methods     | `.len()`, `.upper()`, `.lower()`, `.trim()`, `.split()`, `.contains()`, `.replace()` |
| Control flow       | `if/then/end`, `for x in list/end`, `match expr with/end`    |
| Pipe               | `data \|> func()`                                            |
| Concurrency        | `spawn f()`, `channel(cap)`, `select ... end`, `context()`, `sleep(ms)` |
| Parallel           | `parallel ... end`                                           |
| Algebraic effects  | `perform Name(arg)`, `handle Name { ... } { ... resume(...) }` |
| Dyn dispatch & OO  | `expr as dyn Name`, `dyn Name<T>`                            |
| Modules            | `import "path"`, `export let/task`                           |
| Types              | `type Name = TargetType`                                     |
| Enums & structs    | `enum Name ... end`, `struct Name ... end`                   |
| Macros             | `macro name(params) ... end`                                 |

## Type checking

```mora
let name: string = "mora"
let count: number = 42

task add(a: number, b: number): number
  return a + b
end

let total = add(count, 8)
let inc = fn(x: number) x + 1 end
print(inc(total))
```

Type checking runs automatically before execution and on `mora --check <file>` (emitting `Type error at line N: ...` on mismatches).

## Algebraic effects

```mora
let total = handle Flip {
  let a = perform Flip()
  let b = perform Flip()
  a + b
} {
  resume(1i) + resume(10i)
}

print(total)
```

## Standard library builtins

| Namespace   | Highlights |
|-------------|------------|
| `json.*`    | `json.parse(text)`, `json.stringify(value)` |
| `file.*`    | `read_text`, `write_text`, `append_text`, `read_bytes`, `write_bytes`, `exists`, `is_file`, `is_dir`, `size`, `list`, `mkdir`, `remove`, `rename`, `copy`, `touch`, `cwd`, `chdir`, `home_dir`, `join`, `abs`, `basename`, `dirname`, `extname` |
| `exec.*`    | `exec.run(...)`, `exec.parallel(...)` |
| `math.*`    | scalar math helpers (`abs`, `floor`, `sqrt`, `pow`, trigonometry, `is_nan`, ...) |
| `stats.*`   | `sum`, `mean`, `median`, `histogram`, ... |
| `linalg.*`  | `norm`, `transpose`, `matmul`, `cross`, ... |
| `random.*`  | deterministic / ambient PRNG helpers |

Additional std workspace crates live under `std/` (including `std/math/bigint` / `mora-std-bigint`).

## LSP & editor support

`mora-lsp` speaks JSON-RPC 2.0 over stdio and provides diagnostics (`publishDiagnostics`), hover, completion, go-to-definition, references, rename, document symbols, formatting, semantic tokens, and folding ranges.

Smoke check:

```bash
cargo run --release --example lsp_smoke -- ./target/release/mora-lsp
```

See [editors/](editors/) for editor integrations (`vscode/`, `neovim/`, `helix/`, `sublime/`, `vim/`, `emacs/`).

## Architecture & modules

```
.mora ──► lexer ──► parser_v3::compile ──► MirWitness[] + ControlProgram SSA IR
        ──► typeck (unified Span-tracked HM solver) ──► mir::control optimize (--opt=0/1/2)
        ──► runtime::vm (stackful VM) + runtime::scheduler / runtime::reactor (Tokio)
                ├──► traits / algebraic effects / closures / delimited continuations
                ├──► json.* / file.* (FsPolicy + CheckedPath) / exec.* / math.* / stats.* / linalg.* / random.*
                └──► Value::External(Arc<dyn HostObject>) host extensions
                                ▼
                LSP (mora-lsp) ──► stdio transport + JSON-RPC + providers
```

Key modules under `src/`:

- `lexer.rs` — tokenizer (Int/Float precision, numeric underscores, scientific notation, width suffixes)
- `parser_v3/` — iterative Pratt expression parser and single-pass Control SSA witness compiler
- `mir/control/` — SSA IR, lowering helpers, verifier, and optimizer (`OptLevel`)
- `typeck/` — unified Span-tracked Hindley-Milner solver (`src/typeck/check_mir.rs`, `src/typeck/hm/`) and `import` module symbol verification
- `runtime/` — stackful VM (`vm/`), `CoreRuntime`, `EngineShared`, `TaskScheduler` / `MoraChannel` / `AsyncIoReactor` (`scheduler.rs`, `reactor.rs`), built-in function/method domain handlers (`builtins/`), and `FsPolicy` / `CheckedPath` confinement (`fs.rs`)
- `value/` & `value.rs` — `Value` (including `Channel`, `TaskHandle`, `Context`, `External(Arc<dyn HostObject>)`), chunked persistent `list.rs`, lexical `Environment`, and `display.rs`
- `flow/` & `flow.rs` — shared iterative RFC 8259 JSON parser/serializer and unified numeric tower
- `lsp/` — bounded stdio transport, JSON-RPC router, and HM-unified LSP providers
- `cli/` — shared CLI compilation, interactive REPL (`repl.rs`), and path helpers
- `error.rs` — unified `MoraError` / `MoraResult`

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

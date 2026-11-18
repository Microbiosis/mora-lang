# Mora on MoonBit

Copyright (c) 2026 Microbiosis. BSD-3-Clause. The license at the repository root covers this directory.

This is a **trial** implementation of [Mora](../README.md), written in MoonBit. It does not replace the Rust compiler. `cargo build`, `cargo test`, and the `mora` / `mora-lsp` binaries at the repository root are unchanged.

The trial implements the functional core that the README quick start and “language at a glance” actually exercise: lexing, an iterative Pratt parser, Hindley–Milner type checking, and a tree-walk evaluator. It is not a port of Control SSA, the stackful VM, or the Tokio scheduler.

Toolchain this tree was written against: `moon 0.1.20261117`, `moonc v0.10.21`. The module targets native only.

## Build and run

From this directory:

```bash
moon check --target native
moon test --target native
moon build --target native
```

`preferred_target` is `native`, so `--target native` can be omitted when that default is in effect.

Run a script (the binary is the package `cmd/mora`):

```bash
moon run --target native cmd/mora -- examples/glance.mora
moon run --target native cmd/mora -- examples/typed.mora
moon run --target native cmd/mora -- examples/core.mora
moon run --target native cmd/mora -- ../examples/hm_basic_demo.mora
moon run --target native cmd/mora -- --check examples/typed.mora
moon run --target native cmd/mora -- --version
```

Arguments after `--` are the Mora CLI arguments. A release binary is also produced by `moon build`:

```bash
./_build/native/release/build/cmd/mora/mora.exe examples/glance.mora
```

On this toolchain the native executable name ends in `.exe` even on Linux.

`print` writes to stdout. The startup banner and diagnostics go to stderr, matching the Rust CLI. Parse and type errors exit 2. Runtime and I/O errors exit 1.

`mora` with no arguments, or `mora --repl`, starts a line-oriented REPL. `exit` or `quit` leaves it. A chunk is executed when `task` / `fn` / `if` / `match` / `for` / `while` are balanced by `end`.

## In scope

These programs are the acceptance set. They run under this implementation:

| Program | What it covers |
| --- | --- |
| `examples/glance.mora` | README “language at a glance”: `task`, `return`, `fn`, `list.map`, `match` / `when` / `_` |
| `examples/typed.mora` | README type-checking example: `string`, `number`, annotated `task`, annotated `fn` |
| `examples/core.mora` | Pipe-as-method, `filter` / `reduce`, strings, dicts, `for` / `while` / `assign`, `range`, `and` / `or` / `not`, `**`, recursion |
| `../examples/hm_basic_demo.mora` | The repository’s HM demo: literals, arithmetic, closures, let-polymorphism, lists, homogeneous dicts, `str` |

Semantics that are implemented, not stubbed:

- `--` comments, string escapes, integer and float literals (including `_` separators, exponents, and `i` / `u` / `f` width suffixes). A `n` bigint suffix is rejected. Integer literals that do not fit in `i64` are rejected.
- `let` (optionally annotated), `assign`, `task`, `fn`, `return`, `if` / `then` / `else` / `end`, `match` / `with` (and the `{ ... }` form), `for` / `in`, `while`, `break`, `continue`.
- Operators, with the same precedence as `ParserV3`: `or`, `and`, `==` / `!=`, `|>`, comparisons, `+` / `-`, `*` / `/` / `%`, right-associative `**`, prefix `-` and `not` / `!`.
- `|>` on a call `name(...)` is a method call on the left-hand value (`nums |> map(f)` is `nums.map(f)`). `|>` on any other expression calls that value with the left-hand value.
- Values: `int` (`i64`, checked overflow), `float` (`f64`), `bool`, `string`, `char`, `nil`, lists, dicts with string keys, closures, named tasks.
- `int` stays `int` under `+` `-` `*` `/` `%` `**` (division truncates toward zero). `int` mixed with `float` promotes to `float` when the integer is exactly representable. `+` concatenates when either side is a string. Equal-length lists added together combine elementwise; other list `+` concatenates. Other arithmetic operators broadcast over lists.
- Lists: `map`, `filter`, `reduce`, `len`, `get`, `push` (returns a new list), and indexing. Dicts: `get` (missing key is `nil`), `set` (returns a new dict), `keys`, `values`, `len`. Strings: `len` (Unicode scalar values), `upper`, `lower`, `trim`, `contains`, `starts_with`, `ends_with`, `split`, `replace` (all non-overlapping matches). Case conversion follows MoonBit’s ASCII `to_upper` / `to_lower`.
- Builtins: `print` (tab-separated, returns `nil`), `str`, `len`, `range`.
- Types: `int`, `float`, `number` (`int` or `float`; `int` widens to `float`), `string`, `bool`, `nil`, `char`, `any`, `list<T>`, `dict<K, V>`. Let-generalization uses the value restriction. Annotated parameters and results are checked by subtyping; unannotated functions are inferred. `if` conditions, `and` / `or`, and `when` guards must be `bool`. `and` / `or` are short-circuit and return `bool`.
- Display matches the Rust `Value` formatter for the values above, including `5.0` for integral floats and `[1, 2]` / `{a: 1, b: 2}` (dict keys sorted).

## Not ported

These are rejected with an explicit diagnostic, or are absent from the CLI. They are not silently accepted with different behavior.

- Algebraic effects and delimited continuations: `perform`, `handle`, `resume`, effect signatures.
- Concurrency: `spawn`, `channel`, `select`, `context`, `sleep`, `task_handle`, the Tokio scheduler, `parallel` / `worker`.
- Modules and separate compilation: `import`, `export`.
- Macros, `quote` / quasiquote, `eval` of source text.
- Nominal types: `type`, `enum`, `struct`, traits, `dyn`, `::` paths.
- Pattern forms beyond literals, bindings, and `_` (no list, dict, or tuple patterns).
- Matrix multiplication `@`.
- SSA / MIR, `--opt`, the bytecode VM, and copy-and-patch JIT. `--opt` exits 2 and names the Rust compiler.
- `mora install`, `mora-lsp`, and editor tooling.
- The standard-library namespaces `json.*`, `file.*`, `exec.*`, `math.*`, `stats.*`, `linalg.*`, `random.*`, and `std/math/bigint`.
- Union types other than the `number` abbreviation, `result`, effect rows, and polymorphic recursion.
- `if` without `else` used as a value. As a statement it runs for effect (including `return` / `break` / `continue`) and yields `nil`. A value-position `if` without `else` is a type error; write both branches.
- Unicode case mapping beyond ASCII. `upper` / `lower` use MoonBit’s ASCII conversion.

The Rust implementation remains the source of truth for everything in that list.

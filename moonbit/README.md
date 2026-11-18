# Mora on MoonBit

Copyright (c) 2026 Microbiosis. BSD-3-Clause. The license at the repository root covers this directory.

This is a **trial** implementation of [Mora](../README.md), written in MoonBit. It does not replace the Rust compiler. `cargo build`, `cargo test`, and the `mora` / `mora-lsp` binaries at the repository root are unchanged.

A second trial, written in Mora itself and hosted by that Rust compiler, lives in [`../selfhost`](../selfhost/README.md). This MoonBit tree does not host it.

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

## Linux ARM64 cross-build

From an x86_64 Linux host, this produces a dynamically linked glibc binary for `aarch64-linux-gnu` (ELF `e_machine` 183). MoonBit’s native cross-compile is a compiler selected by `MOON_CC`, not a second `--target`. `MOON_CC` is a program path. Moon runs `cc -dumpmachine`. When that triple’s architecture, OS, or ABI differs from the host, this Moon version (`0.1.20261117`) does not link the host objects `libmoonbitrun.o`, the SIMDUTF objects, or `libbacktrace.a`. The runtime then uses the system allocator, the scalar UTF path, and panics do not print a backtrace. A package-level `cc` does not retarget the shared runtime, so it cannot cross-compile. See [Native build configuration](https://moonbitlang.github.io/moon/native.html#cross-compiling).

Prerequisites on Debian/Ubuntu:

```bash
sudo apt-get install gcc-aarch64-linux-gnu binutils-aarch64-linux-gnu
```

Build:

```bash
cd moonbit
export MOON_CC=aarch64-linux-gnu-gcc
export MOON_AR=aarch64-linux-gnu-ar
"$MOON_CC" -dumpmachine    # aarch64-linux-gnu
moon build --target native --release --target-dir _build/aarch64-linux-gnu
```

The same commands are `scripts/cross-linux-arm64.sh`. The binary is

`_build/aarch64-linux-gnu/native/release/build/cmd/mora/mora.exe`

It is gitignored with the rest of `_build/`. Check the header:

```bash
file _build/aarch64-linux-gnu/native/release/build/cmd/mora/mora.exe
readelf -h _build/aarch64-linux-gnu/native/release/build/cmd/mora/mora.exe
```

A successful build reports `ELF 64-bit LSB pie executable, ARM aarch64` and `Machine: AArch64`. The ELF header field `e_machine` is 183. `readelf -d` lists `libm.so.6`, `libc.so.6`, and `ld-linux-aarch64.so.1`.

Smoke it with qemu-user and the cross sysroot (install `qemu-user-static`):

```bash
qemu-aarch64-static -L /usr/aarch64-linux-gnu \
  _build/aarch64-linux-gnu/native/release/build/cmd/mora/mora.exe \
  examples/glance.mora
```

Measured on this x86_64 Linux VM with `gcc-aarch64-linux-gnu` 13 and `qemu-aarch64-static`: `e_machine` was 183, `--version` printed `Mora v0.110.0-moonbit`, `print(1 + 2)` printed `3`, `--check examples/typed.mora` printed `ok`, and `examples/glance.mora` printed `doubled =	[2, 4, 6, 8]` and `positive`.

What this build is not:

- Not musl. `moonrockz/zig-cross`’s `linux-arm64` target is `aarch64-linux-musl`. This recipe is glibc `aarch64-linux-gnu`. A device needs an aarch64 glibc dynamic linker, or qemu’s `-L` sysroot.
- Not armv7 and not Android. Those triples are not produced here. The published zig-cross target list has no armv7 or Android NDK entry. Do not treat a successful aarch64-linux-gnu link as support for them.
- Not a Windows or macOS binary. The `platform` cfg and the prebuilt `core` library still follow the host. A Linux host and a Linux ARM64 target share the Linux cfg, so `OsString` paths and arguments stay in the Linux form. A Windows binary built on Linux would still see the host cfg and is not a shippable artifact. Do not set `MOON_CC` to a Windows cross compiler on this host.
- Not a second MoonBit backend. `--target native` stays native; only the C compiler changes. Host `moon test` still runs on x86_64.

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

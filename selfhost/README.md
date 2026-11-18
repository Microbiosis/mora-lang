# Mora in Mora

Copyright (c) 2026 Microbiosis. BSD-3-Clause. The license at the repository root covers this directory.

This is a trial of Mora's functional core, written in Mora. It does not replace the Rust compiler, and it does not change the MoonBit trial under `moonbit/` except for a pointer in that tree's README. The Linux ARM64 cross-compile script is untouched.

The pipeline is source → lexer → iterative Pratt parser → Hindley–Milner → tree-walk evaluator. The host that executes this file is the Rust `mora` binary. The MoonBit `mora` trial cannot host it: this source uses `dict<string, any>`, `list<any>`, `type_of`, and `file.read_text` / `file.write_text`, which that trial rejects.

## Build the host

From the repository root, with Rust stable (edition 2024; CI uses current stable):

```bash
rustup run stable cargo build --release -p mora --bin mora
```

That writes `target/release/mora`. Override the host with `MORA_HOST` if the binary lives somewhere else.

## Run

From the repository root:

```bash
selfhost/mora.sh moonbit/examples/glance.mora
selfhost/mora.sh moonbit/examples/typed.mora
selfhost/mora.sh moonbit/examples/core.mora
selfhost/mora.sh examples/hm_basic_demo.mora
selfhost/mora.sh --check moonbit/examples/typed.mora
selfhost/mora.sh --boot moonbit/examples/typed.mora
selfhost/mora.sh --version
```

`selfhost/mora.sh` writes one or more frames to `selfhost/.argv` (gitignored), runs `target/release/mora selfhost/mora.mora`, and forwards the trial's diagnostic file to stderr. The Rust host still prints its own banner on stderr. Program output is the trial's stdout. A frame is a mode line and a path line. `--boot` writes two frames separated by `---`: the outer frame is `run selfhost/mora.mora`, and the inner frame is the command you passed. The outer copy pops its frame, rewrites `.argv` to what remains, and only then evaluates the next program, so the inner copy does not reread the outer command.

`--check` prints `No type errors found. (N expressions)` and does not evaluate. `N` is the number of top-level statements. On the four programs above it matches the Rust host's witness count. A type error exits 2. The trial reports the first type error and then `1 type error(s) found.` Parse errors also exit 2. Runtime errors exit 1.

## What was run

Against `target/release/mora` on this tree, stdout matched the host byte for byte:

| Program | Run | `--check` |
| --- | --- | --- |
| `moonbit/examples/glance.mora` | `doubled =` then the doubled list, then `positive` | `No type errors found. (6 expressions)` |
| `moonbit/examples/typed.mora` | `51` | `No type errors found. (6 expressions)` |
| `moonbit/examples/core.mora` | pipe, filter, reduce, strings, dicts, for/while, range, and/or/not, `**`, `fact`, continue | `No type errors found. (33 expressions)` |
| `examples/hm_basic_demo.mora` | the host demo transcript, including `apply_twice(add_one)(10) = 12` and `compute(4) = 121` | `No type errors found. (43 expressions)` |

`let name: string = 1` exits 2 with the same diagnostic text the host prints for that program, including the line and column and `1 type error(s) found.`

`perform Flip()` is rejected with `algebraic effects (perform/handle) is not in this trial`. `json.parse` is rejected as a stdlib namespace. `selfhost/mora.sh --opt=1` exits 2 without invoking the interpreter; `--opt` belongs to the Rust SSA pipeline.

## Bootstrap

Stage 1 is the Rust host running this file, which runs an example. Stdout matched `target/release/mora` byte for byte, and `--check` matched the host line, for all four programs:

```bash
selfhost/mora.sh moonbit/examples/glance.mora
selfhost/mora.sh --check moonbit/examples/typed.mora
```

Stage 2 is the host running this file, which runs this file again, which runs the example. The same four programs matched again, including `--check`:

```bash
selfhost/mora.sh --boot moonbit/examples/glance.mora
selfhost/mora.sh --boot moonbit/examples/typed.mora
selfhost/mora.sh --boot moonbit/examples/core.mora
selfhost/mora.sh --boot examples/hm_basic_demo.mora
selfhost/mora.sh --boot --check moonbit/examples/typed.mora
```

`--boot --check` prints the same `No type errors found. (N expressions)` line as the host (6, 6, 33, and 43 for the four programs). Each stage-2 run typechecks this file on the host before the inner interpreter starts, so it takes longer than stage 1. On this machine the four stage-2 runs were about one to three minutes each.

The self-check of this file still passes:

```bash
selfhost/mora.sh --check selfhost/mora.mora
```

That prints `No type errors found. (179 expressions)`. Tasks may call tasks defined later; the checker binds every task signature before it checks bodies.

Environment frames are atoms. An assignment inside a called task updates the frame the caller still holds, which is what lets the inner interpreter keep its lexer position and its type environment. A closure keeps those same atoms, so it sees a later assignment to a variable in a frame it captured.

## In scope

Same surface as the programs above, checked and then evaluated:

- `--` comments, strings, character literals, ints and floats (`_` separators, exponents, `i` / `u` / `f` suffixes). A trailing `n` bigint suffix is rejected.
- `let` with an optional annotation, `name = expr` and `assign name = expr`, `task`, `fn`, `return`, `if` / `then` / `else` / `end`, `match` / `with` and the `{ ... }` form, `for` / `in`, `while`, `break`, `continue`.
- Pratt operators: `or`, `and`, `==` / `!=`, `|>`, comparisons, `+` / `-`, `*` / `/` / `%`, right-associative `**`, prefix `-` and `not` / `!`.
- `|>` whose right-hand side is a call `name(...)` is a method call. Lists, string-keyed dicts, indexing.
- `int` stays `int` under arithmetic. Mixed `int` and `float` become `float`. `+` concatenates when either side is a string. Equal-length list `+` follows the host (elementwise); other list `+` concatenates.
- List `map`, `filter`, `reduce`, `len`, `get`, `push`. Dict `get`, `set`, `keys`, `values`, `len` (keys sorted by the host). String `len`, `upper`, `lower`, `trim`, `contains`, `starts_with`, `ends_with`, `split`, `replace`.
- Builtins `print` (tab-separated), `str`, `len`, `range`, `int`, `float`, `type_of`, `atom`, `deref`, and `swap`. `atom` / `deref` / `swap` are how environment frames stay shared across calls.
- Types: `int`, `float`, `number` (`int` widens to `float` and to `number`), `string`, `bool`, `nil`, `char`, `any`, `list<T>`, `dict<K, V>`. Let-generalization uses the value restriction. `and` / `or` are short-circuit and return `bool`.
- Display of the values above is the host's display, because the evaluator keeps host ints, floats, strings, bools, lists, and dicts and prints them with host `str`.

`file.read_text` and `file.write_text` exist so the driver can read the program and record its exit status. Every other `file.*` method is rejected. `json.*`, `exec.*`, `math.*`, `stats.*`, `linalg.*`, and `random.*` are rejected.

## Not in this trial

Rejected with a diagnostic, or absent from the wrapper. Not accepted under another meaning.

- Algebraic effects: `perform`, `handle`, `resume`.
- Concurrency: `spawn`, `channel`, `select`, `context`, `sleep`, and `parallel` / `worker`.
- Modules: `import`, `export`. This file is one program on purpose, so the self-check does not need them.
- Macros, `quote`, `eval` of source text, `::` paths.
- `type`, `enum`, `struct`, traits, `dyn`.
- List, dict, and tuple patterns. Union annotations other than the `number` abbreviation.
- Matrix multiplication `@`. Bigint (`n`).
- SSA / MIR, `--opt`, the bytecode VM, JIT, `mora install`, `mora-lsp`.
- The REPL (`--repl` exits 2).
- A third stage (this file interpreting this file interpreting this file). Stage 2 is the nesting this trial runs.
- More than the first type error. The Rust host can report several; this trial stops at the first.

The Rust compiler remains the source of truth for that list.

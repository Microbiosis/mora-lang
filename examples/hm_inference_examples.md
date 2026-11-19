# HM Type Inference Examples for Mora

This document accompanies [`examples/hm_basic_demo.mora`](hm_basic_demo.mora) and demonstrates Mora's **unified Span-tracked Hindley-Milner type inference** (`src/typeck/check_mir.rs` + `src/typeck/hm/`).

## Setup

Hindley-Milner type checking runs automatically before execution and on `mora --check <file>`. Run the executable demo script directly:

```bash
cargo run -- examples/hm_basic_demo.mora
cargo run -- --check examples/hm_basic_demo.mora
```

## Examples

### 1. Literal Precision & Basic Type Inference

Unsuffixed integer literals infer as exact 64-bit `int` (`Value::Int`), while floating-point literals (with a decimal point, scientific notation, or float width suffix) infer as `float` (`Value::Float`):

```mora
-- Let bindings are inferred automatically
let x = 42
let y = 3.14
let lr = 1e-4
let w_fp16 = 0.5f16
let message = "Hello, HM!"

-- Binary operations infer result types
let sum = x + 10
let product = y * 2.0
let power = 2 ** 10

-- Comparison operators return bool
let is_positive = x > 0

print(x, y, lr, w_fp16, sum, product, power, is_positive, message)
```

---

### 2. Closures, Let-Polymorphism & Value Restriction

Closures receive fresh type variables for unannotated parameters and generalize via let-polymorphism (subject to ML Value Restriction):

```mora
-- Inferred as (int) -> int from usage
let add_one = fn(n) n + 1 end
print(add_one(5))
print(add_one(10))

-- Polymorphic identity function: forall a. (a) -> a
let identity = fn(x) x end
print(identity(42))
print(identity("hello"))

-- Higher-order function taking a closure
let apply_twice = fn(f, x) f(f(x)) end
print(apply_twice(add_one, 10))
```

---

### 3. Explicit Annotations & Rigid Generic Parameters

Tasks and closures support explicit type annotations and rigid generic parameters (`<T>`, `<T, U>`):

```mora
task id_generic<T>(x: T): T
  return x
end

task add_ints(a: int, b: int): int
  return a + b
end

print(id_generic(100))
print(id_generic("tensor"))
print(add_ints(20, 22))
```

---

### 4. Pattern Matching with Guards

All arms of a `match ... with ... end` expression unify to a common result type, and guard expressions (`when ...`) must unify with `bool`:

```mora
let classify = fn(n: int)
  match n with
    x when x > 100 -> "large"
    x when x < 10 -> "small"
    _ -> "medium"
  end
end

print(classify(42))
```

---

### 5. Homogeneous Containers & Matrix Contraction (`@`)

Lists and dictionaries enforce element/value homogeneity at compile time, and `@` checks vector/matrix dimensions:

```mora
let numbers = [1, 2, 3, 4, 5]          -- list<int>
let doubled = numbers.map(fn(x) x * 2 end)

let weights = [[1.0, 2.0], [3.0, 4.0]] -- list<list<float>>
let input = [0.5, 1.5]                 -- list<float>
let out = weights @ input              -- list<float>

let metrics = {loss: 0.12, acc: 0.98}  -- dict<string, float>
print(doubled, out, metrics)
```

---

### 6. Cloud-Native Concurrency Types (`channel<T>`, `task_handle<T>`, `context`)

`channel<T>` is strictly invariant in `T`, while `task_handle<T>` returned by `spawn` is covariant:

```mora
let ch = channel(1)
let ctx = context()

let h = spawn fn()
  ch.send(7)
  return 42
end

let received = select
  case v = ch.recv() => v
  timeout(100) => 0
end

print(received, h.join(), ctx.is_cancelled())
```

---

### 7. Algebraic Effects & Delimited Continuations

`handle` binds `resume` in the handler block with the continuation type inferred from the performed effect body:

```mora
let result = handle Ask {
  let base = perform Ask()
  base * 2
} {
  resume(21)
}

print(result)
```

---

**Version**: v0.110.0 (Unified Span-Tracked HM Constraint Solver)  
**License**: BSD-3-Clause

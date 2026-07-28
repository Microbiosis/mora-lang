# HM Type Inference Examples for Mora-lang

This directory contains example programs demonstrating the **Hindley-Milner type inference** system in mora-lang.

## Setup

To enable HM inference, set the `MORA_HM` environment variable:

```bash
# Windows PowerShell
$env:MORA_HM="1"; cargo run -- examples/hm_inference.mora

# Unix/macOS
MORA_HM=1 cargo run -- examples/hm_inference.mora
```

## Examples

### 1. Basic Type Inference (`basic_inference.mora`)
Demonstrates automatic type inference for simple expressions:

```moraversion = "v0.53"

# Let bindings are inferred automatically
let x = 42
let y = 3.14
let message = "Hello, HM!"

# Binary operations infer result types
let sum = x + x
let product = y * 2.0

# Comparison operators return bool
let is_positive = x > 0

# All types are inferred without annotations
print(x)        # inferred as int
print(y)        # inferred as float  
print(message)  # inferred as string
```

**Expected behavior**: 
- `x: int` (inferred from literal)
- `y: float` (inferred from literal)
- `sum: int` (int + int → int)
- `product: float` (float * int → float)
- `is_positive: bool` (comparison always returns bool)

---

### 2. Function Closure Inference (`closure_example.mora`)
Shows how closures get inferred function types:

```moraversion = "v0.53"

# Function with inferred parameter and return types
let add_one = num => {
    return num + 1
}

# First call infers num's type as int
add_one(5)

# Second call verifies consistency
add_one(10)

# Higher-order function: identity function
let identity = x => x
identity(42)      # infers arg is int, returns int
identity("hello") # infers arg is string, returns string

# Generic-like behavior via let-generalization
let apply_twice = f => x => {
    return f(f(x))
}

apply_twice(add_one)(10)   # works: (int -> int) applied twice
apply_twice(identity)("hi") # works: polymorphic identity
```

**Key HM concepts demonstrated**:
- Closure parameter types are fresh type variables
- Return type is determined by body expression
- Let-generalization allows polymorphic functions like `identity`

---

### 3. Type Constraint Checking (`constraint_checking.mora`)
Tests the unification algorithm's ability to detect type errors:

```moraversion = "v0.53"

# This should work - both sides are integers
let a = 10 + 5
assert_eq(a, 15)

# This should fail - mixed Int and Float in comparison
# Uncommenting this line will cause a type error:
# let b = 10 == 3.14

# Arithmetic operations require same numeric type
let c = 3.14 + 2.5      # OK: float + float
# let d = 3.14 + 2       # ERROR: mixed float + int

# Boolean expressions
let flag = true
let negated = not(flag)
assert_flag(negated)     # Should be false

# String concatenation (example placeholder)
# let greeting = "Hello" + ", " + "World"
```

---

### 4. Pattern Matching Types (`pattern_matching.mora`)
Demonstrates type inference for pattern matching expressions:

```moraversion = "v0.53"

# Simple value to match on
let number = 42

# Pattern match with guarded arms
let result = match number {
    case n if n > 100:
        "large"
    case n if n < 10:
        "small"
    default:
        "medium"
}

# The result type is unified across all arms
# All three branches return strings, so 'result' is inferred as string
print(result)

# Match on different types
let value = "hello"

let length = match value {
    case s if string_length(s) > 5:
        string_length(s)
    default:
        0
}

# Here we have mixed int results from branches
# HM will unify them successfully
assert_int(length)
```

---

### 5. Recursive Functions (`recursive_func.mora`)
Tests recursive function type inference:

```moraversion = "v0.53"

# Factorial: recursively defined function
let rec factorial = n => {
    if n <= 1 then
        1
    else
        n * factorial(n - 1)
}

# Type inference should deduce: factorial : int -> int
factorial(5)  # Expected: 120

# Fibonacci sequence
let rec fib = n => {
    if n <= 1 then
        n
    else
        fib(n - 1) + fib(n - 2)
}

# Also inferred as: fib : int -> int
fib(10)  # Expected: 55

# Sum of list (with placeholder list operations)
# let rec sum_list = lst => {
#     match lst {
#         case []:
#             0
#         case [head | tail]:
#             head + sum_list(tail)
#     }
# }
```

---

### 6. List Operations (`list_examples.mora`)
Type inference for list constructions and operations:

```moraversion = "v0.53"

# Homogeneous lists get inferred element type
let numbers = [1, 2, 3, 4, 5]  # inferred as list<int>
let floats = [1.0, 2.0, 3.0]   # inferred as list<float>

# Mixed types should fail at compile time:
# let mixed = [1, "two", 3]  # TYPE ERROR!

# List operations preserve element types
let doubled = map(lambda x => x * 2, numbers)  # list<int>

# Single-element lists
let singleton = [42]  # inferred as list<int>

# Empty list type is ambiguous (may need explicit annotation)
# let empty_list: list<int> = []
```

---

### 7. Dictionary Types (`dict_types.mora`)
Inference for dictionary/associative array types:

```moraversion = "v0.53"

# Simple key-value dictionaries
let user = {name: "Alice", age: 30}  
# inferred as dict<string, any> due to heterogeneous values

let prices = {apple: 1.5, orange: 2.0}
# inferred as dict<string, float>

# Uniform value types allow better inference
let ages = {alice: 25, bob: 30, charlie: 35}
# inferred as dict<string, int>

# Nested structures
let student = {
    name: "Bob",
    scores: [85, 90, 78]
}
# inferred as dict<string, any> with nested list<int> in 'scores' field
```

---

### 8. Type Errors Detection (`type_errors.mora`)
Examples that should trigger type inference errors:

```moraversion = "v0.53"

# ERROR: Comparing incompatible types
# let compare_mixed = 10 == 3.14  # Unification failure: int vs float

# ERROR: Using string where number expected  
# let bad_arithmetic = "hello" + 5  # Not a valid operation

# ERROR: Mismatched arity in closure application
# let apply_three_args = func => func(1, 2, 3)
# apply_three_args(x => x * 2)  # Closure takes 1 arg, but 3 provided

# The HM inference system should report these with span information
print("Run this file with MORA_HM=1 to see type errors")
```

---

## Testing HM Inference

### Run all examples:
```powershell
cd examples
$env:MORA_HM="1"
foreach ($file in Get-ChildItem "*.mora") {
    Write-Host "Testing $($file.Name)..."
    cargo run -- $file.FullName
}
```

### Specific test:
```powershell
$env:MORA_HM="1"
cargo run -- hm_basic_inference.mora
```

### Expected outputs:
- Successful runs show inferred types
- Failed runs display clear type error messages with line/column info

---

## Implementation Status

| Feature | Status | Notes |
|---------|--------|-------|
| **Basic Literals** |  | Int, Float, String, Bool, Nil |
| **Let Bindings** | ⏳ | Requires environment tracking |
| **Binary Operators** |  | With numeric constraints |
| **Closures** |  | Parameter/return type inference |
| **Function Calls** |  | Placeholder (needs resolution) |
| **Pattern Match** |  | Arm type unification (partial) |
| **List Literals** | ⏳ | Homogeneity checking needed |
| **Dict Literals** | ⏳ | Key/value type checking needed |
| **Recursive Funcs** | ⏳ | Requires fixpoint combinator |
| **Error Diagnostics** |  | Span-based error reporting |

---

## Technical Notes

### How HM Inference Works

1. **Fresh Type Variables**: Each unknown type gets a fresh variable (α₁, α₂, ...)
2. **Constraint Generation**: Type constraints generated during traversal
3. **Unification**: MGU (Most General Unifier) solves constraints
4. **Let-Generalization**: Polymorphic types from let-bindings
5. **Error Reporting**: When unification fails, emit span-based errors

### Example Trace: `let x = 42 + 5`

```
1. Infer literal 42 → Type::Int
2. Infer literal 5 → Type::Int  
3. Generate constraint: (+ Int, Int) ∈ NumericBinary
4. Apply unification → Result type = Int
5. Generalize x → x : ∀().Int = Int (monomorphic)
```

### Constraints System

```rust
pub enum Constraint {
    Eq(Box<Type>, Box<Type>),           // Two types must be equal
    Numeric(BinaryConstraint),          // Both operands are numeric
}
```

---

## Contributing

To add new examples:
1. Create a `.mora` file in this directory
2. Include version comment at top
3. Add comprehensive comments explaining the concept
4. Document expected type inlines and potential errors
5. Test with `MORA_HM=1` to verify correct behavior

---

**Version**: v0.53 (HM Inference Prototype - Phase β)  
**License**: MIT

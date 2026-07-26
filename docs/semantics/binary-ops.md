# Mora  — 

> ****`a + b`, `a - b`, `a * b`, `a / b`, `a % b` 
> **Property tests**`src/formal_semantics.rs`  `test_binary_*`
> ****`src/flow.rs` (numeric_op, numeric_cmp, eval_binary)

---

## 1. 

Mora  `Int(i64)`  `Float(f64)`  v0.38  Rust-strict v0.53  `Number(f64)`  `Float(f64)` 

****
- `Int`  → `Int`  `+`, `-`, `*`, `%` 
- `Float`  → `Float` 
- `Int` + `Float`  `Float` + `Int`→ ****

---

## 2. 

###  BIN-INT-ADD

****`i1`, `i2`  `Int`

****
```
Γ ⊢ Int(i1) + Int(i2) ⇒ Int(i1 + i2)
```

###  BIN-INT-SUB

****`i1`, `i2`  `Int`

****
```
Γ ⊢ Int(i1) - Int(i2) ⇒ Int(i1 - i2)
```

###  BIN-INT-MUL

****`i1`, `i2`  `Int`

****
```
Γ ⊢ Int(i1) * Int(i2) ⇒ Int(i1 * i2)
```

###  BIN-INT-DIV

****`i1`, `i2`  `Int``i2 ≠ 0`

****
```
Γ ⊢ Int(i1) / Int(i2) ⇒ Int(i1 / i2)    i2 ≠ 0
```

###  BIN-INT-DIV-ZERO

****`i1`, `i2`  `Int``i2 = 0`

****
```
Γ ⊢ Int(i1) / Int(0) ⇒ Error("division by zero")
```

###  BIN-INT-MOD

****`i1`, `i2`  `Int``i2 ≠ 0`

****
```
Γ ⊢ Int(i1) % Int(i2) ⇒ Int(i1 % i2)    i2 ≠ 0
```

###  BIN-FLOAT-ADD

****`f1`, `f2`  `Float`

****
```
Γ ⊢ Float(f1) + Float(f2) ⇒ Float(f1 + f2)
```

###  BIN-FLOAT-OP

****`f1`, `f2`  `Float``op ∈ {+, -, *, /, %}`

****
```
Γ ⊢ Float(f1) op Float(f2) ⇒ Float(f1 op f2)
```

###  BIN-MIXED-ERROR

**** `Int` `Float`

****
```
Γ ⊢ Int(i) op Float(f) ⇒ Error("mixed Int and Float operands")
Γ ⊢ Float(f) op Int(i) ⇒ Error("mixed Int and Float operands")
```

 `op ∈ {+, -, *, /, %}`

###  BIN-COMPARE-INT

****`i1`, `i2`  `Int``cmp ∈ {==, !=, <, >, <=, >=}`

****
```
Γ ⊢ Int(i1) cmp Int(i2) ⇒ Bool(i1 cmp i2)
```

###  BIN-COMPARE-FLOAT

****`f1`, `f2`  `Float`

****
```
Γ ⊢ Float(f1) cmp Float(f2) ⇒ Bool(f1 cmp f2)
```

###  BIN-COMPARE-MIXED-ERROR

**** `Int` `Float`

****
```
Γ ⊢ Int(i) cmp Float(f) ⇒ Error("mixed Int and Float operands")
Γ ⊢ Float(f) cmp Int(i) ⇒ Error("mixed Int and Float operands")
```

---

## 3. Property Tests

| Property |  |
|----------|------|
| `P-BIN-INT-CLOSE` | `∀ i1, i2: Int. i1 + i2`  `Int` |
| `P-BIN-FLOAT-CLOSE` | `∀ f1, f2: Float. f1 + f2`  `Float` |
| `P-BIN-MIXED-ERROR` | `∀ i: Int, f: Float. i + f`  |
| `P-BIN-DIV-ZERO-ERROR` | `∀ i: Int. i / 0`  |
| `P-BIN-COMPARE-BOOL` | `∀ v1, v2: numeric. v1 cmp v2`  `Bool` |
| `P-BIN-NAN` | `Float(NaN) + Float(x)`  `Float(NaN)` |
| `P-BIN-OVERFLOW` | `Int(MAX) + 1`  `Int(overflowed)` |

---

## 4. 

Property tests 

1.  `eval_binary``src/flow.rs`
2.  property `Int`  `Float` 
3. 

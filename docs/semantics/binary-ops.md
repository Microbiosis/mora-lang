# Mora 形式化语义 — 二元数值操作（严格模式）

> **构造**：`a + b`, `a - b`, `a * b`, `a / b`, `a % b` 等二元运算符
> **Property tests**：`src/formal_semantics.rs` 中 `test_binary_*`
> **相关代码**：`src/flow.rs` (numeric_op, numeric_cmp, eval_binary)

---

## 1. 定义

Mora 的数值系统严格区分 `Int(i64)` 和 `Float(f64)` 两个类型。这是从 v0.38 引入的 Rust-strict 模式，v0.53 合并 `Number(f64)` 到 `Float(f64)` 后进一步强化。

**类型规则**：
- `Int` 操作数 → `Int` 结果（当操作为 `+`, `-`, `*`, `%` 时）
- `Float` 操作数 → `Float` 结果
- 混合类型（`Int` + `Float` 或 `Float` + `Int`）→ **运行时错误**

---

## 2. 操作语义规则

### 规则 BIN-INT-ADD

**前提**：`i1`, `i2` 是 `Int`

**规则**：
```
Γ ⊢ Int(i1) + Int(i2) ⇒ Int(i1 + i2)
```

### 规则 BIN-INT-SUB

**前提**：`i1`, `i2` 是 `Int`

**规则**：
```
Γ ⊢ Int(i1) - Int(i2) ⇒ Int(i1 - i2)
```

### 规则 BIN-INT-MUL

**前提**：`i1`, `i2` 是 `Int`

**规则**：
```
Γ ⊢ Int(i1) * Int(i2) ⇒ Int(i1 * i2)
```

### 规则 BIN-INT-DIV（整数除法）

**前提**：`i1`, `i2` 是 `Int`，`i2 ≠ 0`

**规则**：
```
Γ ⊢ Int(i1) / Int(i2) ⇒ Int(i1 / i2)    （截断除法，i2 ≠ 0）
```

### 规则 BIN-INT-DIV-ZERO（除以零）

**前提**：`i1`, `i2` 是 `Int`，`i2 = 0`

**规则**：
```
Γ ⊢ Int(i1) / Int(0) ⇒ Error("division by zero")
```

### 规则 BIN-INT-MOD

**前提**：`i1`, `i2` 是 `Int`，`i2 ≠ 0`

**规则**：
```
Γ ⊢ Int(i1) % Int(i2) ⇒ Int(i1 % i2)    （i2 ≠ 0）
```

### 规则 BIN-FLOAT-ADD

**前提**：`f1`, `f2` 是 `Float`

**规则**：
```
Γ ⊢ Float(f1) + Float(f2) ⇒ Float(f1 + f2)
```

### 规则 BIN-FLOAT-OP

**前提**：`f1`, `f2` 是 `Float`，`op ∈ {+, -, *, /, %}`

**规则**：
```
Γ ⊢ Float(f1) op Float(f2) ⇒ Float(f1 op f2)
```

### 规则 BIN-MIXED-ERROR（混合类型错误）

**前提**：一个操作数是 `Int`，另一个是 `Float`

**规则**：
```
Γ ⊢ Int(i) op Float(f) ⇒ Error("mixed Int and Float operands")
Γ ⊢ Float(f) op Int(i) ⇒ Error("mixed Int and Float operands")
```

其中 `op ∈ {+, -, *, /, %}`。

### 规则 BIN-COMPARE-INT

**前提**：`i1`, `i2` 是 `Int`，`cmp ∈ {==, !=, <, >, <=, >=}`

**规则**：
```
Γ ⊢ Int(i1) cmp Int(i2) ⇒ Bool(i1 cmp i2)
```

### 规则 BIN-COMPARE-FLOAT

**前提**：`f1`, `f2` 是 `Float`

**规则**：
```
Γ ⊢ Float(f1) cmp Float(f2) ⇒ Bool(f1 cmp f2)
```

### 规则 BIN-COMPARE-MIXED-ERROR

**前提**：一个操作数是 `Int`，另一个是 `Float`

**规则**：
```
Γ ⊢ Int(i) cmp Float(f) ⇒ Error("mixed Int and Float operands")
Γ ⊢ Float(f) cmp Int(i) ⇒ Error("mixed Int and Float operands")
```

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-BIN-INT-CLOSE` | `∀ i1, i2: Int. i1 + i2` 结果是 `Int` |
| `P-BIN-FLOAT-CLOSE` | `∀ f1, f2: Float. f1 + f2` 结果是 `Float` |
| `P-BIN-MIXED-ERROR` | `∀ i: Int, f: Float. i + f` 报错 |
| `P-BIN-DIV-ZERO-ERROR` | `∀ i: Int. i / 0` 报错 |
| `P-BIN-COMPARE-BOOL` | `∀ v1, v2: numeric. v1 cmp v2` 结果是 `Bool` |
| `P-BIN-NAN` | `Float(NaN) + Float(x)` 结果是 `Float(NaN)`（或报错） |
| `P-BIN-OVERFLOW` | `Int(MAX) + 1` 结果是 `Int(overflowed)`（实现相关） |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 用 `eval_binary`（`src/flow.rs`）直接构造输入值
2. 对每个 property，随机生成 `Int` 或 `Float` 值
3. 断言结果类型和值符合语义规则

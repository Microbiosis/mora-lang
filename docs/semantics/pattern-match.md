# Mora 形式化语义 — 模式匹配

> **构造**：`match <expr> with <pattern> => <body> ... end` 及表达式匹配
> **Property tests**：`src/semantics_tests.rs` 中 `test_match_*`
> **相关代码**：`src/interpreter/execute.rs` (execute_match), `src/interpreter/evaluate.rs` (match_pattern)

---

## 1. 定义

Mora 支持语句级和表达式级模式匹配。模式匹配将值与一系列模式比较，第一个匹配的模式执行其 body，并将模式中的变量绑定到匹配的值。

**语法**：
```
match <expr>
  | <pattern1> => <body1>
  | <pattern2> => <body2>
  ...
end
```

**模式种类**：
- `Wildcard` (`_`)：匹配任何值，不绑定
- `Variable(name)`：匹配任何值，绑定到 `name`
- `Literal(lit)`：只匹配等于 `lit` 的值
- `List { prefix, rest }`：匹配列表，前缀模式匹配前 N 个元素，`rest` 绑定剩余部分
- `Dict([(k₁, p₁), ...])`：匹配字典，每个键的模式必须匹配
- `Guard { pattern, condition }`：匹配 `pattern` 且 `condition` 为真

---

## 2. 操作语义规则

### 规则 MATCH-WILDCARD（通配符）

**前提**：任意值 `v`

**规则**：
```
∀ v: Value.  v matches _    （匹配成功，无绑定）
```

### 规则 MATCH-VARIABLE（变量绑定）

**前提**：`name` 是标识符，`v` 是任意值

**规则**：
```
∀ v: Value, ∀ name: Ident.
  v matches name    且  bindings = {name ↦ v}
```

### 规则 MATCH-LITERAL（字面量匹配）

**前提**：`lit_val` 是字面量对应的 `Value`

**规则**：
```
v matches Literal(lit) ⇔ v == lit_val
（匹配成功无绑定，失败则不匹配）
```

### 规则 MATCH-LIST（列表匹配）

**前提**：`v = List([e₁, ..., eₙ])`，`pats = [p₁, ..., pₖ]`

**规则**（无 rest）：
```
n = k  且  ∀ i. eᵢ matches pᵢ
⇐⇒ List([e₁,...,eₙ]) matches List([p₁,...,pₖ])
```

**规则**（有 rest）：
```
n ≥ k  且  ∀ i ∈ [1,k]. eᵢ matches pᵢ
⇐⇒ List([e₁,...,eₙ]) matches List([p₁,...,pₖ], rest=r)
    且  r = List([eₖ₊₁,...,eₙ])
```

### 规则 MATCH-DICT（字典匹配）

**前提**：`v = Dict(map)`，`pats = [(k₁, p₁), ..., (kₙ, pₙ)]`

**规则**：
```
∀ i ∈ [1,n]. (kᵢ ∈ keys(map)) 且  map[kᵢ] matches pᵢ
⇐⇒ Dict(map) matches Dict([(k₁,p₁), ..., (kₙ,pₙ)])
```

### 规则 MATCH-GUARD（守卫条件）

**前提**：`v matches pattern`，`Γ ⊢ condition ⇒ Bool(true)`

**规则**：
```
v matches pattern  且  Γ ⊢ condition ⇒ Bool(true)
⇐⇒ v matches Guard{pattern, condition}
```

**规则**（守卫失败）：
```
v matches pattern  且  Γ ⊢ condition ⇒ Bool(false)
⇐⇒ v 不匹配 Guard{pattern, condition}
```

### 规则 MATCH-FIRST-WINS（首个匹配优先）

**前提**：`v matches pᵢ` 且 `∀ j < i. v 不匹配 pⱼ`

**规则**：
```
v matches p₁ => body₁
...
v matches pₖ => bodyₖ
────────────────────────────────────────
v matches arms[i]    （执行 body[i]，不执行 body[i+1..k]）
```

### 规则 MATCH-NO-MATCH（无匹配）

**前提**：`∀ i. v 不匹配 pᵢ`

**规则**：
```
∀ i. v 不匹配 pᵢ
────────────────────────────
v matches arms ⇒ Nil    （无匹配，结果为 Nil，无绑定）
```

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-MATCH-WILDCARD` | `_` 匹配任何值 |
| `P-MATCH-VARIABLE` | `x` 匹配任何值并绑定 |
| `P-MATCH-LITERAL` | `Literal(l)` 只匹配等于 `l` 的值 |
| `P-MATCH-LIST-EXACT` | `[a, b]` 只匹配长度为 2 的列表 |
| `P-MATCH-LIST-REST` | `[a, ...rest]` 匹配长度 ≥ 1 的列表，`rest` 绑定尾部 |
| `P-MATCH-DICT` | `{k: p}` 要求字典有键 `k` 且值匹配 `p` |
| `P-MATCH-FIRST-WINS` | 第一个匹配的模式执行 body，后面的不执行 |
| `P-MATCH-NO-MATCH` | 无匹配时结果为 `Nil` |
| `P-MATCH-GUARD-TRUE` | 守卫条件为真时匹配 |
| `P-MATCH-GUARD-FALSE` | 守卫条件为假时不匹配 |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 构造 Mora 源码（含 match 语句）
2. 在 body 中设置不同的环境变量（如 `let marker = "arm1"`）
3. 解析 + 类型检查 + 解释执行
4. 断言哪个 arm 被执行、绑定是否正确

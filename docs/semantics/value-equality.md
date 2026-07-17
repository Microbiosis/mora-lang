# Mora 形式化语义 — Value 相等性

> **构造**：`Value` 类型的 `==` / `!=` 操作
> **Property tests**：`src/formal_semantics.rs` 中 `test_value_eq_*`
> **相关代码**：`src/value.rs` (Value 定义 + PartialEq impl)

---

## 1. 定义

`Value` 是 Mora 运行时的所有值的统称。当前有 27 个变体（见 `src/value.rs`），包括：

- 原语：`String`, `Char`, `Int`, `Float`, `Bool`, `Nil`
- 容器：`List`, `Dict`, `Tuple`
- 可调用：`Task`, `Closure`, `Tool`, `Builtin`
- AI 相关：`Conversation`, `Stream`, `AiConfig`, `AiResult`, `Agent`
- 系统：`Router`, `HttpRequest`, `HttpResponse`, `McpServer`, `Result_`, `Macro`, `Compose`, `Partial`

---

## 2. 操作语义规则

### 规则 EQ-REFLEXIVE（自反性）

**前提**：`v` 是一个 `Value`

**规则**：
```
∀ v: Value.  v == v
```

### 规则 EQ-SYMMETRIC（对称性）

**前提**：`v1`, `v2` 是 `Value`

**规则**：
```
∀ v1, v2: Value.  (v1 == v2) ⇔ (v2 == v1)
```

### 规则 EQ-TRANSITIVE（传递性）

**前提**：`v1`, `v2`, `v3` 是 `Value`

**规则**：
```
∀ v1, v2, v3: Value.  (v1 == v2) ∧ (v2 == v3) ⇒ (v1 == v3)
```

### 规则 EQ-TYPE-DISJOINT（类型不相交）

**前提**：`v1` 和 `v2` 是不同的 `Value` 变体

**规则**：
```
∀ v1, v2: Value.  variant(v1) ≠ variant(v2) ⇒ v1 ≠ v2
```

### 规则 EQ-PRIMITIVE（原语逐位相等）

**前提**：`v1`, `v2` 是同一原语变体

**规则**：
```
∀ s1, s2: String.    String(s1) == String(s2) ⇔ s1 == s2
∀ c1, c2: Char.      Char(c1) == Char(c2) ⇔ c1 == c2
∀ i1, i2: Int.       Int(i1) == Int(i2) ⇔ i1 == i2
∀ f1, f2: Float.     Float(f1) == Float(f2) ⇔ f1 == f2
∀ b1, b2: Bool.      Bool(b1) == Bool(b2) ⇔ b1 == b2
Nil == Nil           (总是 true)
```

### 规则 EQ-LIST（列表按元素相等）

**前提**：`v1`, `v2` 都是 `List`

**规则**：
```
∀ ls1, ls2: Vec<Value>.
  List(ls1) == List(ls2) ⇔ (|ls1| == |ls2|) ∧ (∀i < |ls1|. ls1[i] == ls2[i])
```

### 规则 EQ-DICT（字典按键值对相等）

**前提**：`v1`, `v2` 都是 `Dict`

**规则**：
```
∀ d1, d2: HashMap<String, Value>.
  Dict(d1) == Dict(d2) ⇔ (∀k ∈ keys(d1) ∪ keys(d2). d1[k] == d2[k])
  （其中 k 不存在时视为不存在的键，不存在 ≠ 任何值）
```

### 规则 EQ-NAN（NaN 特殊处理）

**前提**：`v` 是 `Float` 且为 NaN

**规则**：
```
Float(NaN) != Float(NaN)  （NaN 不等于任何值，包括自身）
```

### 规则 EQ-TASK（Task 按名称相等）

**前提**：`t1`, `t2` 都是 `Task`

**规则**：
```
Task(t1) == Task(t2) ⇔ t1.name == t2.name
（参数列表不参与比较）
```

### 规则 EQ-CLOSURE（Closure 引用相等）

**前提**：`c1`, `c2` 都是 `Closure`

**规则**：
```
Closure(c1) == Closure(c2) ⇔ c1 与 c2 是同一个 Arc 实例
```

---

## 3. 后验性质（Property Tests）

以下性质应在 `src/formal_semantics.rs` 中用 proptest 验证：

| Property | 断言 |
|----------|------|
| `P-EQ-REFLEXIVE` | `∀ v: Value. v == v` |
| `P-EQ-SYMMETRIC` | `∀ v1, v2: Value. (v1 == v2) ⇔ (v2 == v1)` |
| `P-EQ-TRANSITIVE` | `∀ v1, v2, v3: Value. (v1 == v2) ∧ (v2 == v3) ⇒ (v1 == v3)` |
| `P-EQ-TYPE-DISJOINT` | `∀ v1, v2: Value. variant(v1) ≠ variant(v2) ⇒ v1 ≠ v2` |
| `P-EQ-LIST-LENGTH` | `∀ ls1, ls2: Vec<Value>. len(ls1) ≠ len(ls2) ⇒ List(ls1) ≠ List(ls2)` |
| `P-EQ-DICT-SUPERSET` | `∀ d1, d2: Dict. (∀k ∈ keys(d1). d2[k] == d1[k]) ⇒ (d1 == d2 ∧ keys(d1) ⊆ keys(d2))` |
| `P-EQ-NAN` | `Float(NaN) != Float(NaN)` |
| `P-EQ-NIL-REFLEXIVE` | `Nil == Nil` |

---

## 4. 实现验证

验证以上性质的方法：

1. 构造一个 `proptest` 策略，生成随机的 `Value` 实例
2. 对每个 property，随机生成输入，断言性质成立
3. 用 `#[test] fn test_value_eq_reflexive() { ... }` 等函数包装

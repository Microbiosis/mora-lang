# Mora 形式化语义 — For 循环迭代

> **构造**：`for <var> in <iterable> ... end`
> **Property tests**：`src/formal_semantics.rs` 中 `test_for_*`
> **相关代码**：`src/interpreter/execute.rs` (execute_for)

---

## 1. 定义

`for` 循环遍历一个可迭代对象（列表、字典、字符串），每次迭代将当前元素绑定到循环变量。

**语法**：
```
for <var>[: <type>] in <iterable>
  <body>
end
```

---

## 2. 操作语义规则

### 规则 FOR-LIST（列表遍历）

**前提**：
1. `Γ ⊢ iterable ⇒ List([e₀, e₁, ..., eₙ₋₁])`
2. 对于每个 `i`，`Γ ⊢ body[i] ⇒ rᵢ`，其中 `body[i]` 是在 `Γ, var ↦ eᵢ` 下执行 body

**规则**：
```
Γ ⊢ iterable ⇒ List([e₀, ..., eₙ₋₁])
∀ i < n. Γ, var ↦ eᵢ ⊢ body ⇒ rᵢ
─────────────────────────────────────────────────────────────
Γ ⊢ for var in iterable body end ⇒ [r₀, r₁, ..., rₙ₋₁]
```

### 规则 FOR-EMPTY（空列表）

**前提**：`Γ ⊢ iterable ⇒ List([])`

**规则**：
```
Γ ⊢ iterable ⇒ List([])
────────────────────────────────────────
Γ ⊢ for var in iterable body end ⇒ []
```

### 规则 FOR-BREAK（提前退出）

**前提**：
1. 在某次迭代 `i`，`body[i]` 执行 `break`
2. 之前的迭代 `j < i` 正常执行

**规则**：
```
∀ j < i. Γ, var ↦ eⱼ ⊢ body ⇒ rⱼ
Γ, var ↦ eᵢ ⊢ body ⇒ Break
───────────────────────────────────────────────────────
Γ ⊢ for var in iterable body end ⇒ [r₀, ..., rᵢ₋₁]
```

### 规则 FOR-CONTINUE（跳过本次）

**前提**：
1. 在某次迭代 `i`，`body[i]` 执行 `continue`
2. 后续的迭代正常继续

**规则**：
```
∀ j < i. Γ, var ↦ eⱼ ⊢ body ⇒ rⱼ
Γ, var ↦ eᵢ ⊢ body ⇒ Continue
∀ j > i. Γ, var ↦ eⱼ ⊢ body ⇒ rⱼ
─────────────────────────────────────────────────────────────────────
Γ ⊢ for var in iterable body end ⇒ [r₀, ..., rᵢ₋₁, rᵢ₊₁, ..., rₙ₋₁]
（结果列表跳过索引 i）
```

### 规则 FOR-VAR-SCOPE（循环变量作用域）

**前提**：`var` 在循环体外已有绑定

**规则**：
```
Γ ⊢ var ⇒ v_old
Γ, var ↦ e ⊢ body ⇒ r
────────────────────────────────────────────────
Γ ⊢ for var in iterable body end ⇒ result
其中 result 在原始环境 Γ 下可见 var 的旧值 v_old
```

循环变量在循环体内遮蔽外部同名变量，循环结束后恢复旧值。

### 规则 FOR-ITEM-VALUE（迭代项的值）

**前提**：`Γ ⊢ iterable ⇒ List([e₀, ..., eₙ₋₁])`

**规则**：
```
第 i 次迭代时，Γ ⊢ var ⇒ eᵢ
```

循环变量在每次迭代中被赋值为当前列表元素。

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-FOR-ITEMS` | `for x in [a, b, c]` 执行 body 3 次，`x` 分别为 `a`, `b`, `c` |
| `P-FOR-EMPTY` | `for x in [] body end` 执行 body 0 次 |
| `P-FOR-BREAK` | `break` 后循环立即终止，不执行剩余迭代 |
| `P-FOR-CONTINUE` | `continue` 后跳过本次 body 剩余部分，继续下次迭代 |
| `P-FOR-SCOPE` | 循环外的 `var` 值不受循环体内修改影响 |
| `P-FOR-LEN` | `len(for x in lst collect x end)` == `len(lst)` |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 构造 Mora 源码（含 for 语句）
2. 在 body 中记录每次迭代的 `var` 值和计数
3. 解析 + 类型检查 + 解释执行
4. 断言计数和值符合语义规则

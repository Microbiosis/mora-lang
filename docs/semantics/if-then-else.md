# Mora 形式化语义 — If-Then-Else 条件

> **构造**：`if <condition> then <then_body> [else <else_body>] end`
> **Property tests**：`src/formal_semantics.rs` 中 `test_if_*`
> **相关代码**：`src/interpreter/execute.rs` (execute_if)

---

## 1. 定义

`if` 语句根据条件的真假执行不同分支。

**语法**：
```
if <condition> then
  <then_body>
[else
  <else_body>]
end
```

---

## 2. 操作语义规则

### 规则 IF-THEN（条件为真）

**前提**：
1. `Γ ⊢ condition ⇒ Bool(true)`
2. `Γ ⊢ then_body ⇒ result`

**规则**：
```
Γ ⊢ condition ⇒ Bool(true)
Γ ⊢ then_body ⇒ result
───────────────────────────────────────
Γ ⊢ if condition then then_body end ⇒ result
```

### 规则 IF-ELSE-TRUE（带 else，条件为真）

**前提**：
1. `Γ ⊢ condition ⇒ Bool(true)`
2. `Γ ⊢ then_body ⇒ result`

**规则**：
```
Γ ⊢ condition ⇒ Bool(true)
Γ ⊢ then_body ⇒ result
─────────────────────────────────────────────────────
Γ ⊢ if condition then then_body else else_body end ⇒ result
```

### 规则 IF-ELSE-FALSE（带 else，条件为假）

**前提**：
1. `Γ ⊢ condition ⇒ Bool(false)`
2. `Γ ⊢ else_body ⇒ result`

**规则**：
```
Γ ⊢ condition ⇒ Bool(false)
Γ ⊢ else_body ⇒ result
─────────────────────────────────────────────────────
Γ ⊢ if condition then then_body else else_body end ⇒ result
```

### 规则 IF-NO-ELSE-FALSE（无 else，条件为假）

**前提**：`Γ ⊢ condition ⇒ Bool(false)`

**规则**：
```
Γ ⊢ condition ⇒ Bool(false)
────────────────────────────────────────
Γ ⊢ if condition then then_body end ⇒ Nil
```

### 规则 IF-CONDITION-TYPE-ERROR

**前提**：`Γ ⊢ condition ⇒ v`，且 `v` 不是 `Bool`

**规则**：
```
Γ ⊢ condition ⇒ v  且  v ∉ {Bool(true), Bool(false)}
─────────────────────────────────────────────────────────────
Γ ⊢ if condition then ... end ⇒ Error("if condition must be bool")
```

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-IF-THEN-EXECUTES` | 条件为真时，`then_body` 被执行，`else_body` 不被执行 |
| `P-IF-ELSE-EXECUTES` | 条件为假且存在 else 时，`else_body` 被执行 |
| `P-IF-NO-ELSE-NIL` | 条件为假且无 else 时，结果为 `Nil` |
| `P-IF-COMP-STRONG` | 条件为真时，结果为 `then_body` 的值 |
| `P-IF-COMP-FALSE` | 条件为假时，结果为 `else_body` 的值（有 else）或 `Nil`（无 else） |
| `P-IF-NON-BOOL-ERROR` | 非布尔条件报错 |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 构造 Mora 源码（含 if 语句）
2. 解析 + 类型检查 + 解释执行
3. 在 then/else 分支中设置不同的环境变量（如 `let marker = "then"` / `let marker = "else"`）
4. 断言哪个分支被执行

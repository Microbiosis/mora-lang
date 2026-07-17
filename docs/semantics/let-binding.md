# Mora 形式化语义 — Let 绑定与变量查找

> **构造**：`let x: T = expr` 语句和变量查找
> **Property tests**：`src/formal_semantics.rs` 中 `test_let_*`
> **相关代码**：`src/interpreter/execute.rs` (execute_let), `src/value.rs` (Environment)

---

## 1. 定义

`let` 语句将变量名绑定到一个值，并在后续语句中可被查找。

**语法**：
```
let <name>[: <type_hint>] = <expr>
```

- `name`：变量名（标识符）
- `type_hint`：可选的类型提示（当前仅用于文档/类型检查，不改变运行期行为）
- `expr`：初始化表达式，在当前环境中求值

---

## 2. 操作语义规则

### 规则 LET-BIND（绑定）

**前提**：
1. `Γ` 是当前的环境映射
2. `expr` 在 `Γ` 下求值得到 `v`
3. `name` 是一个标识符

**规则**：
```
Γ ⊢ expr ⇒ v
────────────────────────────
Γ, let name = v ⊢ body ⇒ result    （body 在扩展环境中执行）
```

其中 `Γ, name ↦ v` 表示将 `name` 映射到 `v` 后得到的新环境。

### 规则 LET-SCOPE（作用域）

**前提**：`name` 在 `Γ` 中已有绑定

**规则**：
```
Γ ⊢ name ⇒ v_old
Γ' = Γ + {name ↦ v_new}
────────────────────────────────────────
Γ' ⊢ name ⇒ v_new    （新绑定遮蔽旧绑定）
```

### 规则 LET-READ-ONLY（只读性）

**前提**：`x` 通过 `let` 绑定

**规则**：
```
Γ ⊢ let x = v ⊢ assign x = v' ⇒ Error("x is immutable")
```

`let` 绑定的变量不可通过 `assign` 修改（除非用 `assign` 语句显式声明可变，但 Mora 中 `let` 默认不可变）。

### 规则 LET-ORDER（顺序性）

**前提**：同一作用域内有多个 `let` 语句

**规则**：
```
Γ ⊢ let x = v1 ⊢ body1 ⇒ result1
Γ ⊢ let y = v2 ⊢ body2 ⇒ result2
──────────────────────────────────────────
Γ ⊢ let x = v1; let y = v2 ⊢ body ⇒ body 在 {x↦v1, y↦v2} 中执行
```

`let` 语句按顺序执行，后一个 `let` 可以看到前一个 `let` 的绑定。

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-LET-READ` | `let x = v` 后，`x` 求值得到 `v` |
| `P-LET-SHADOW` | 内层 `let x = v2` 遮蔽外层 `let x = v1`，内层访问 `x` 得到 `v2` |
| `P-LET-ORDER` | 顺序绑定时，后一个绑定可以看到前一个 |
| `P-LET-IMMUTABLE` | `let x = v` 后 `assign x = v'` 报错 |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 构造 Mora 源码（含 `let` 语句）
2. 解析 + 类型检查 + 解释执行
3. 断言运行期结果符合语义规则

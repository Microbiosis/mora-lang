# Mora 形式化语义 — 函数调用（Task）

> **构造**：`task` 定义与调用 `f(args...)`
> **Property tests**：`src/semantics_tests.rs` 中 `test_fn_*`
> **相关代码**：`src/interpreter/dispatch.rs` (call_function, call_task_inner)

---

## 1. 定义

`task` 是 Mora 中的命名函数。调用时按位置传参，参数绑定到 task body 的作用域。

**语法**：
```
task <name>(<params>) [: <return_type>]
  <body>
end
```

调用：
```
<name>(<arg1>, <arg2>, ...)
```

---

## 2. 操作语义规则

### 规则 FN-DEFINE（定义）

**前提**：
1. `params` 是参数名列表 `[p₁, ..., pₙ]`
2. `body` 是语句列表 `[s₁, ..., sₖ]`
3. `name` 是标识符

**规则**：
```
──────────────────────────────────────────────────────
Γ ⊢ task name(p₁, ..., pₙ) body end ⇒ Γ, name ↦ Task(name, [p₁,...,pₙ], body)
```

### 规则 FN-CALL-OK（调用成功）

**前提**：
1. `Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)`
2. `Γ ⊢ args[i] ⇒ aᵢ`，`i = 1..n`
3. `Γ' = Γ + {p₁↦a₁, ..., pₙ↦aₙ}`
4. `Γ' ⊢ body ⇒ r`（body 最后一句的返回值）

**规则**：
```
Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)
∀ i ∈ [1,n]. Γ ⊢ args[i] ⇒ aᵢ
Γ, p₁↦a₁, ..., pₙ↦aₙ ⊢ body ⇒ r
───────────────────────────────────────────────────
Γ ⊢ name(args[1], ..., args[n]) ⇒ r
```

### 规则 FN-CALL-ARITY-ERROR（参数数量错误）

**前提**：`Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)`，`args` 的长度 `m ≠ n`

**规则**：
```
Γ ⊢ name ⇒ Task(name, [p₁,...,pₙ], body)
m = len(args), m ≠ n
────────────────────────────────────────────────────────────
Γ ⊢ name(args[1], ..., args[m]) ⇒ Error("task expects n args, got m")
```

### 规则 FN-RETURN（返回值）

**前提**：body 中最后一条语句为 `return <expr>` 或 body 最后表达式的值

**规则**：
```
body = [s₁, ..., sₖ₋₁, return e]
Γ ⊢ e ⇒ r
────────────────────────────────────────
Γ ⊢ body ⇒ r
```

### 规则 FN-RETURN-IMPLICIT（隐式返回）

**前提**：body 中没有 `return` 语句

**规则**：
```
body = [s₁, ..., sₖ]  （无 return）
sₖ 的求值结果为 r
──────────────────────────────────────
Γ ⊢ body ⇒ r
```

### 规则 FN-ARG-EVAL-ORDER（参数求值顺序）

**前提**：`args = [a₁, ..., aₙ]`

**规则**：
```
参数按从左到右顺序求值：
Γ ⊢ args[1] ⇒ v₁, Γ ⊢ args[2] ⇒ v₂, ..., Γ ⊢ args[n] ⇒ vₙ
```

### 规则 FN-CALL-VALUE（内置函数调用）

**前提**：`name` 是内置函数名（`print`, `len`, `range` 等）

**规则**：
```
Γ ⊢ print(v₁, ..., vₙ) ⇒ Nil    （副作用：打印）
Γ ⊢ len(v) ⇒ Int(len(v))         （v 是 list/dict/string）
Γ ⊢ range(start, end, step) ⇒ List(start, start+step, ..., <end)
```

---

## 3. 后验性质（Property Tests）

| Property | 断言 |
|----------|------|
| `P-FN-RETURN` | `task f() return v end; f()` 结果为 `v` |
| `P-FN-PARAM-BIND` | `task f(x) return x end; f(a)` 结果为 `a` |
| `P-FN-ARITY-ERROR` | `task f(x, y) ... end; f(a)` 报错 |
| `P-FN-ARG-EVAL-LEFT-TO-RIGHT` | 参数按从左到右顺序求值 |
| `P-FN-IMPLICIT-RETURN` | 无 `return` 的 task 返回 body 最后表达式的值 |
| `P-FN-NIL-RETURN` | 空 body 或只有语句的 task 返回 `Nil` |
| `P-FN-BUILTIN-LEN` | `len([1,2,3])` → `Int(3)` |
| `P-FN-BUILTIN-RANGE` | `range(0, 3)` → `[0, 1, 2]` |

---

## 4. 实现验证

Property tests 通过以下步骤验证：

1. 构造 Mora 源码（含 task 定义和调用）
2. 解析 + 类型检查 + 解释执行
3. 断言返回值、参数绑定、错误消息符合语义规则

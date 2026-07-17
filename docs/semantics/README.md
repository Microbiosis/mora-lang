# Mora 形式化语义 — 根目录

> 每个构造一个小步操作语义规则 + 对应的 property test。
> 语义钉死后，编译器优化和 AI 生成代码才能被严格静态检查。

## 目录

| 构造 | 语义文档 | Property Test |
|------|---------|---------------|
| Value 相等性 | [`value-equality.md`](value-equality.md) | `formal_semantics::test_value_eq_*` |
| Let 绑定与变量查找 | [`let-binding.md`](let-binding.md) | `formal_semantics::test_let_*` |
| 二元数值操作（严格模式） | [`binary-ops.md`](binary-ops.md) | `formal_semantics::test_binary_*` |
| If-Then-Else 条件 | [`if-then-else.md`](if-then-else.md) | `formal_semantics::test_if_*` |
| For 循环迭代 | [`for-loop.md`](for-loop.md) | `formal_semantics::test_for_*` |
| 函数调用（Task） | [`function-call.md`](function-call.md) | `formal_semantics::test_fn_*` |
| 模式匹配 | [`pattern-match.md`](pattern-match.md) | `formal_semantics::test_match_*` |
| Tool 声明语法 | [`tool-declaration.md`](tool-declaration.md) | `formal_semantics::test_tool_*` |
| Tool 类型系统 | [`tool-type-system.md`](tool-type-system.md) | `formal_semantics::test_tool_*` |

## 语义规则格式

每条规则采用以下格式：

```
[规则名]
前提条件：
  1. <前提 1>
  2. <前提 2>

语义规则：
  Γ, x ↦ v ⊢ expr ⇒ result    (环境 Γ 下 expr 求值得到 result)

后验性质（Property）：
  ∀ ... . <性质断言>
```

## 与路线图的关系

- 本文档对应 **METAMORPHOSIS_ROADMAP.md** 中的**相变 β（形式化）**
- 每个语义规则对应一个 property test（proptest），CI 跑 spec-derived property test
- β 完成标志：所有核心构造（上述 7 个）都有语义文档 + property test

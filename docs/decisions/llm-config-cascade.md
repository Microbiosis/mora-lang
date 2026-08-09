# Decision Log: LLM Config Cascade（v0.76.01 草案）

**Date**: 2026-08-08
**Version**: v0.76.01 草案
**Status**: Active（采纳制，不强制）
**Source**: 借鉴 NVIDIA NeMo Labs-OO-Agents (NOOA) 5 级 cascade

## 背景

v0.76.00 NOOA 借鉴调研（@stone-from-other-hills 触发）确认一条可借鉴智慧：
NOOA 的 `INHERIT` 哨兵 + 5 级 cascade（instance → class MRO → parent agent → error）让 LLM 配置覆盖关系显式化。

Mora 当前 AI 原语（`route / observe / record / replay`）的配置是**隐式来源**：
- 写死在字面量里
- 或从 env 变量读
- 或从 `project.toml`（v0.34+）

调用方需要"猜"哪个来源生效——cascade 缺失带来**配置不可预测**。

## 目标

让 `route / observe / record / replay` 字面量的 config（max_steps / model / timeout / cache_key 等）按**显式 5 级 cascade** 解析：

```
1. inline literal       `route { max_steps: 5, model: "gpt-4" }`         最高
2. call-site override   `route(default, override={max_steps: 10})`       ↑
3. module default       在 module 顶部 `config record { max_steps: 3 }`  ↑
4. project.toml         `[record]\nmax_steps = 2`                       ↑
5. env var              `MORA_RECORD_MAX_STEPS=1`                      最低
```

任意级未指定 → fallback 到下一级；所有级都未指定 → `MoraError::ConfigNotFound`。

## 采纳制（不强制）

按 `docs/decisions/no-borrowed-constraints.md` 原则：

- ✅ **不引入借鉴项目硬约束**——cascade 不是 Mora 必须
- ✅ **不引入时间盒**——你拍板时才实现
- ✅ **保留 escape hatch**——env var 兜底永远可用

如果你**不想采纳**——本文档作为"知道这条路，但选了别的"记录，仍入仓。

## 关键设计决策

### 决策 1：5 级 cascade（沿用 NOOA 5 级）

**理由**：NOOA 的 5 级是经验值（MRO + instance + parent + scope + default）——Mora 把它映射到 Mora 概念：

| NOOA 概念 | Mora 对应 |
|---|---|
| instance | **inline literal**（最高优先级）|
| class MRO | **call-site override**（函数参数覆盖）|
| parent agent | **module default**（`config record { ... }`）|
| error | → cascade 跳到下一级 |

新增 2 级（Mora 专属）：
- **project.toml**（编译期已读）
- **env var**（运行时最低兜底）

**为何不照搬 NOOA 的 4 级**？Mora 无 class 概念，缺一层 MRO；补 project.toml + env var 是 Mora 工程化必备。

### 决策 2：fallback 报错用 MoraError

**MoraError 变体**：
- `MoraError::ConfigNotFound(String)`（v0.76.01 新增）

**统一错误路径**——延续 v0.75.98-99 + v0.76.00 的 MoraError 统一计划（当前 9/267 处覆盖）。

### 决策 3：AIBound 编译期标签

**借鉴 NOOA 的 `AgentMeta` 元类检测 `...` 标记**——Mora 翻译为：

```rust
// src/mir/inst.rs 新增：
pub enum MirInstKind {
    // ... 现有 variant
    /// v0.76.02: AI-bound 标记——route/observe/record/replay 字面量专用
    /// JIT 优化可走独立 fallback path
    AIBound {
        config: ConfigCascade,
    },
}
```

**为何不直接复用 NOOA 的 metaclass `__new__` 重写**？按 AGENTS.md §7：
- NOOA 的 `__new__` 重写是**Python 类 + 元类**机制——Mora 是不存在类与元类的自有语言
- 引入类与元类 = "type-as-value 歧义" + "破坏 HM 推断纯净性"
- 改成**纯 MIR 内部标签**——不污染语法层，只动 runtime 决策

### 决策 4：`doc()` progressive disclosure（Mora 版）

**借鉴 NOOA 的 `doc(obj)` 在 LLM REPL 命名空间可用**——Mora 翻译为：

```rust
// src/builtin/mod.rs 新增：
/// v0.76.02+: 暴露给 LLM 决策的 progressive disclosure 原语
/// 返回 obj 的 public 字段 + 类型签名（与 LSP hover 同源）
pub fn doc(obj: Any) -> Doc
```

**为何不抄 NOOA 的 `Annotated[T, hidden]` 标签协议**？按 AGENTS.md §7：
- 标签协议靠类型注解的元数据，脆弱
- Mora 用**显式关键字** `pub / internal / ai-visible`（**草案**，未实现）更清晰

## 风险评估

| 风险 | 等级 | 检测 | 缓解 |
|---|---|---|---|
| **cascade 与 MoraError 统一进度冲突** | 🟡 | MoraError 当前 9/267 处覆盖，cascading 配置可能引入新错误维度 | 必须先完成 MoraError 统一到 80%+ 再做 cascade |
| **AIBound 标签被滥用** | 🟢 | JIT 路径可监控错误率 | 加 `MoraError::Internal` 防御性检查 |
| **doc() LSP 同步成本** | 🟡 | LSP hover 与 doc() 共享 schema，schema 改动两处同步 | 抽出 `mora::schema::describe(obj)` 内部 helper |
| **cascade 与现有 env var 兼容** | 🟢 | env var 已支持，新 cascade 是叠加层 | 现有 env var 读路径不变 |
| **设计草案过拟合 NOOA 抽象** | 🟡 | NOOA 是 Python 宿主语言，**被迫取舍**；Mora 走语言层设计 | 任何 NOOA 借鉴必须先回答"不借鉴能否解决" |

## 未变

- 现有 `route / observe / record / replay` 字面量**签名不变**
- 现有 env var 读路径不变
- 现有 7 个独立 Error 类型不变
- 现有 HM 推断 + 双向定型不变

## 下一步（不在本次范围）

- v0.76.02: 实现 `MoraError::ConfigNotFound` + AIBound 标签（如采纳）
- v0.76.03: 实现 cascade 解析（如采纳）
- v0.76.04: 实现 `doc()` builtin（如采纳）
- v0.76.05: 测试覆盖（AGENTS.md §3「测试同步原则」）

## 关联决策

- `docs/decisions/no-borrowed-constraints.md`（v0.75.93）—— 不引入借鉴项目硬约束
- `docs/decisions/diag-filter-extraction.md`（v0.75.94）—— DiagFilter 抽象参考
- 架构审查报告（v0.75.90）—— 风险矩阵 + 三层穿透模型

## 关闭条件

本文档在以下任一情况关闭（更新状态为"Closed"）：
- cascade / AIBound / doc() 全部实现 → 关闭为 "Implemented in v0.76.x"
- 作者明确决定不采纳 → 关闭为 "Rejected"
- 4 年未动 → 自动关闭为 "Stale"
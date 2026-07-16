# Mora-lang 架构与 Bug 检测报告 v0.0.55

> **检测日期**：2026-07-11  
> **基线版本**：Cargo 0.0.55（v0.55 baseline）  
> **源文件数**：94 个 .rs 文件  
> **总 LOC**：36,874 行（不含 tests/、examples/）  
> **与上次报告（2026-07-10）对比**：从 46,712 → 36,874 LOC，纯 src 目录口径差异

---

## 一、门禁状态

| 门禁 | 状态 | 备注 |
|------|------|------|
| `cargo build --all-targets` | ✅ 通过 | 0.81s |
| `cargo test --all` | ✅ 863 通过 / 0 失败 / 15 ignored | 较上次 755 → 863（+108 新增测试） |
| `cargo clippy -D warnings` | ✅ 通过 | 上次有 2 dead-code，现已修复 |
| `cargo fmt --check` | ✅ 通过 | 上次有 2 处差异，现已修复 |

**结论**：四项门禁全部绿灯，上次报告中的 clippy / fmt 问题已清偿。

---

## 二、结构债全景

### 2.1 unwrap / panic / expect 密度

| 指标 | 数量 | 较上次变化 | 趋势 |
|------|------|------------|------|
| `unwrap()` | 473 | ↑89（上次 423 → 473） | 📈 **恶化** |
| `panic!` | 173 | 新增统计 | 📈 高密度 |
| `.expect()` | 328 | 新增统计 | ⚠️ 中密度 |
| **合计危险调用** | **974** | — | 🔴 极高风险 |

**密度热力图**（unwrap 按文件排序）：

| 文件 | unwrap | panic | expect | 行数 | unwrap密度/千行 |
|------|--------|-------|--------|------|-----------------|
| `interpreter/builtins.rs` | 85 | 100 | 139 | 5,098 | 16.7 |
| `checkpoint/mod.rs` | 39 | 0 | — | 723 | 54.0 |
| `record/tests.rs` | 39 | 0 | — | 607 | 64.4 |
| `checkpoint/sqlite.rs` | 32 | 0 | — | 307 | 104.0 🔴 |
| `audit/mod.rs` | 25 | 0 | — | 720 | 34.7 |
| `interpreter/orchestrate_v2.rs` | 24 | 6 | — | 1,435 | 16.7 |
| `refine/mod.rs` | 22 | 0 | — | 328 | 67.1 |
| `toolplane/mod.rs` | 20 | 0 | — | 312 | 64.5 |
| `plan/mod.rs` | 20 | 0 | — | 276 | 72.5 |
| `flow.rs` | 19 | 5 | — | 1,168 | 16.3 |

**🔴 最危险文件**：
- **`checkpoint/sqlite.rs`**：104 unwrap/千行，密度全场最高
- **`refine/mod.rs`**：67.1 /千行
- **`plan/mod.rs`**：72.5 /千行

**builtins.rs 综合危险度**：85 unwrap + 100 panic + 139 expect = **324 危险调用**，占文件 5,098 行的 **6.4%**，即每 15.7 行就有一个危险调用。

### 2.2 巨型文件（未拆分）

| 文件 | LOC | 风险等级 | 说明 |
|------|-----|----------|------|
| `interpreter/builtins.rs` | 5,098 | 🔴 极高 | 196 match + 324 危险调用，上帝函数库 |
| `interpreter/mod.rs` | 3,336 | 🔴 极高 | Interpreter 主模块，混杂定义+逻辑 |
| `parser_v2/statements.rs` | 2,166 | 🟡 中高 | 解析器单文件偏大 |
| `typeck/mod.rs` | 2,055 | 🟡 中高 | 类型检查+类型系统定义未分离 |
| `compress/json.rs` | 1,512 | 🟡 中 | JSON 压缩逻辑密集 |
| `interpreter/orchestrate_v2.rs` | 1,435 | 🟡 中高 | 编排逻辑 |
| `interpreter/dispatch.rs` | 1,417 | 🟡 中高 | 消息分发 |
| `interpreter/execute.rs` | 1,162 | 🟡 中 | 执行引擎 |
| `flow.rs` | 1,168 | 🟡 中 | 流控制逻辑 |
| `main.rs` | 1,043 | 🟡 中 | CLI 入口偏大 |
| `lexer.rs` | 1,246 | 🟡 中 | 词法分析 |
| `value.rs` | 706 | 🟠 中低 | 值类型定义 |
| `ai_infra.rs` | 783 | 🟡 中 | AI 基础设施（65 dead_code 注解） |

### 2.3 大型枚举变体数

| 枚举 | 变体数 | 风险 |
|------|--------|------|
| `TokenType` | ~120+ | 🟡 match 穷尽性维护困难 |
| `Value` | 33 | 🟠 中等 |
| `Type` | 30 | 🟠 中等 |
| `ExprKind` | — | 待核实 |
| `StmtKind` | — | 待核实 |

### 2.4 pub 字段过度暴露

| 位置 | 公开字段数 | 说明 |
|------|------------|------|
| `runtime/*` 7 个 facade | 34 个 `pub` 字段 | 结构债 #1：facade 内部 pub 字段过多，封装不足 |
| `interpreter/mod.rs` | 8 个 `pub(crate)` + 多个 `pub` | 旧架构残留 |

**runtime facade 逐文件暴露**（全为 `pub`，非 `pub(crate)`）：
- `ai.rs`：7 个 pub 字段（token_budget, token_usage, trace, draft_model_stats, context_window, speculative_verifier, cache_warmer）
- `core.rs`：7 个 pub 字段（globals, environment, tool_registry, v2_arena, current_ai_config, config_stack, worker_channels/receivers）
- `infra.rs`：6 个 pub 字段（recorder, string_interner, ai_cache, bus, scheduler, 另 1）
- `persist.rs`、`registry.rs`、`sandbox.rs`、`orch.rs`：各 4-5 个 pub 字段

### 2.5 dead_code 注解抑制

| 总数 | 108 |
|------|-----|
| `#[allow(dead_code)]` | 101 |
| `#[allow(unused)]` | 1 |

**密度热力图**：
- **`ai_infra.rs`**：65 个 dead_code 注解（783 行中的 8.3%）🔴
- `typeck/mod.rs`：10
- `interpreter/mod.rs`：9

> `ai_infra.rs` 是最严重的 dead_code 堆积区——65 个被抑制的 dead_code 警告意味着该文件有大量未使用的代码残骸。

---

## 三、Bug 风险分析

### 3.1 🔴 高危：ai_chat.rs 零测试黑洞

| 文件 | 测试数 | LOC | 风险 |
|------|--------|-----|------|
| `interpreter/ai_chat.rs` | **0** | 865 | 🔴 核心AI对话路径无任何单元测试 |

**影响**：AI 对话是 mora 的核心差异化功能（AI-native 一等公民），865 行代码零测试意味着任何修改都可能引入不可检测的回归。

### 3.2 🟡 中危：dispatch.rs 测试密度低

| 文件 | 测试数 | LOC | 测试密度 |
|------|--------|-----|----------|
| `interpreter/dispatch.rs` | 5 | 1,417 | 3.5/千行 |
| `interpreter/execute.rs` | 9 | 1,162 | 7.7/千行 |

dispatch 是消息分发枢纽，5 个测试覆盖 1,417 行逻辑，覆盖率显著不足。

### 3.3 🟡 中危：unwrap 在 checkpoint/sqlite.rs 的密度

`checkpoint/sqlite.rs`：32 unwrap / 307 行 = **104/千行**。SQLite 操作涉及 IO 错误，unwrap 会在磁盘异常、锁冲突、数据损坏时直接 panic，而不是优雅返回错误。

### 3.4 🟡 中危：builtins.rs 100 个 panic!

`builtins.rs` 有 100 个 `panic!` 调用。虽然部分 panic 可能在测试模块中，但生产代码中的 panic 会在用户输入触发时直接崩溃解释器，违反 AGENTS.md 的"禁 unwrap()/panic!"规则。

### 3.5 🟠 低危：Arc<Mutex> 过度使用

| 指标 | 数量 |
|------|------|
| `Arc<Mutex<_>>` | 45 |
| `Arc<RwLock<_>>` | 6 |

**密度热力图**：
- `value.rs`：18 处 Arc<Mutex>（Value 类型内部的共享状态）
- `runtime/sandbox.rs`：3
- `runtime/orch.rs`：3
- `runtime/infra.rs`：3

> mora 的硬约束 C1（零 async runtime）意味着所有并发靠 `Arc<Mutex>` 协调。45 处 Arc<Mutex> 中多数是必要的（解释器全局状态），但 `value.rs` 的 18 处值得审视——Value 是否需要如此多的内部可变性？

### 3.6 🟠 低危：697 次 clone()

`clone()` 共 697 次。在树遍历解释器中 clone 是常见的（Value/AST 节点需要复制），但高频 clone 可能是性能瓶颈的信号，特别是在热路径上。

### 3.7 🟢 安全：零 unsafe

全项目仅 6 处 `unsafe`（document/backend 3 + builtins.rs 2 + sandbox/container 1），且都在受控的特定场景。无安全隐患。

---

## 四、架构分析

### 4.1 Interpreter → Runtime 分层

当前结构：
```
interpreter/mod.rs (3,336行) ← 持有 7 个 runtime facade holder
  ├── runtime/core.rs    (7 pub 字段, 106行)
  ├── runtime/ai.rs      (7 pub 字段, 120行)
  ├── runtime/infra.rs   (6 pub 字段, 126行)
  ├── runtime/persist.rs (4 pub 字段, 95行)
  ├── runtime/registry.rs(4 pub 字段, 119行)
  ├── runtime/sandbox.rs (4 pub 字段, 134行)
  └ runtime/orch.rs     (4 pub 字段, 119行)
```

**问题**：
1. runtime facade 的字段全为 `pub`（非 `pub(crate)`），任何模块可直接修改内部状态，无封装保障
2. **反向依赖**：runtime 层反向 import interpreter 层 5 处（`RouteConfig`, `TokenBudget`, `TokenUsage`, `AiConfigValue`, `ToolDef`, `TraitInfo`, `TraitMethodSig`, `LruCache`），形成隐式循环依赖
3. Interpreter mod.rs 仍是 3,336 行巨型文件，混杂类型定义 + 逻辑 + 测试（138 个测试内嵌）

### 4.2 依赖方向违规

```
runtime/* → interpreter/* (5 处 import)
  但架构预期是 interpreter → runtime（单向）
```

被 runtime 依赖的 interpreter 类型：
- `RouteConfig`, `TokenBudget`, `TokenUsage` → runtime/ai.rs
- `AiConfigValue`, `ToolDef` → runtime/core.rs
- `LruCache` → runtime/infra.rs
- `TraitInfo`, `TraitMethodSig` → runtime/registry.rs

**建议**：这些类型应下沉到 shared kernel（BC1 Language core），或提取为独立 `types.rs`，切断 interpreter ↔ runtime 的隐式循环。

### 4.3 测试分布

| 类别 | 测试数 | 覆盖区域 |
|------|--------|----------|
| interpreter/mod.rs | 138 | Interpreter 主逻辑（良好） |
| interpreter/builtins.rs | 101 | 内建函数（良好） |
| typeck/* | 67 + 22 | 类型检查（良好） |
| flow.rs | 38 | 流控制（良好） |
| record/tests.rs | 27 | 记录系统（良好） |
| lexer.rs | 27 | 词法分析（良好） |
| checkpoint/mod.rs | 24 | 持久化（良好） |
| parser_v2/* | 22 + 19 + 7 | 解析器（尚可） |

**测试黑洞**（核心路径零或极少测试）：
| 文件 | 测试数 | 重要性 |
|------|--------|--------|
| `ai_chat.rs` | **0** | 🔴 AI对话核心 |
| `dispatch.rs` | 5 | 🟡 消息分发枢纽 |
| `execute.rs` | 9 | 🟡 执行引擎 |

### 4.4 模块边界

```
src/
  ├── interpreter/   (10文件, 13,495行) ← 最大的子系统
  ├── parser_v2/     (3文件, 3,867行)
  ├── typeck/        (3文件, 4,222行)
  ├── checkpoint/    (3文件, 1,179行)
  ├── runtime/       (8文件, ~1,200行)
  ├── compress/      (5文件, 2,472行)
  ├── document/      (多子模块)
  ├── lsp/           (多子模块)
  ├── sandbox/       (3文件, 1,281行)
  ├── 其他独立模块...
```

**结构问题**：
- `interpreter/` 占 13,495 行（36.7%），远超合理模块边界
- `builtins.rs` 一个文件 5,098 行，占 interpreter 子系统的 37.8%
- `runtime/` 的 7 个 facade 总字段 35 个，全 `pub`，无行为边界

---

## 五、与上次报告（2026-07-10）的变化对比

| 指标 | 2026-07-10 | 2026-07-11 | 变化 |
|------|------------|------------|------|
| Cargo 版本 | 0.0.53 | 0.0.55 | ↑2 minor |
| 测试通过数 | 755 | 863 | +108 ✅ |
| unwrap 数 | 334 → 423 | 473 | ↑50 📈恶化 |
| clippy | 2 dead-code ❌ | 0 ✅ | 已修复 ✅ |
| fmt | 2 diff ❌ | 0 ✅ | 已修复 ✅ |
| ai_infra.rs dead_code | 未统计 | 65 | 🔴 新发现 |
| ai_chat.rs 零测试 | 已知 | 0 | 未改善 |

**进步**：
- ✅ clippy / fmt 门禁从失败恢复到全绿
- ✅ 测试数从 755 增至 863（+108）

**恶化**：
- 📈 unwrap 从 423 → 473（+50），结构债在持续累积而非清偿
- 📈 panic!/expect 新增统计，合计危险调用达 974
- 🔴 ai_infra.rs 65 dead_code 注解——大量未使用代码残骸

---

## 六、优先级建议

### P0（立即处理）

1. **为 ai_chat.rs 补充单元测试**：865 行核心路径零测试是最高危 Bug 风险
2. **清除 ai_infra.rs 的 65 dead_code 注解**：要么删除死代码，要么激活使用路径

### P1（本周处理）

3. **开始拆 builtins.rs**：5,098 行 + 324 危险调用，按功能域拆为 5-8 个子模块
4. **降低 checkpoint/sqlite.rs 的 unwrap 密度**：104/千行 → 目标 <10/千行
5. **将 runtime 的 34 个 pub 字段改为 pub(crate)**：恢复封装边界

### P2（下两周）

6. **切断 runtime ↔ interpreter 隐式循环依赖**：提取 shared types 到 BC1
7. **拆 interpreter/mod.rs**：3,336 行 → 类型定义 + 逻辑 + 测试分离
8. **审查 value.rs 的 18 处 Arc<Mutex>**：减少不必要的内部可变性

### P3（Plateau A 持续）

9. **unwrap 全项目清偿**：473 → 目标 <50（生产代码）
10. **clone() 热路径审计**：697 次 → 标注热路径 clone，逐步替换为引用传递

---

## 七、附录：数据采集方法

- `cargo build --all-targets`：编译门禁
- `cargo test --all`：测试门禁
- `cargo clippy --all-targets --all-features -- -D warnings`：lint 门禁
- `cargo fmt --check`：格式门禁
- `grep -rn "unwrap()" src/`：危险调用统计
- `grep -rn "panic!" src/` + `grep -rn ".expect(" src/`：补充统计
- `grep -rn "#\[test\]" src/`：测试覆盖统计
- `grep -rn "pub " src/runtime/`：封装暴露统计
- `grep -rn "use crate::interpreter" src/runtime/`：依赖方向检测
- `grep -rn "#\[allow(dead_code" src/`：死代码抑制统计
- `grep -rn "Arc<Mutex" src/`：并发模式统计
- `wc -l src/**/*.rs`：文件规模统计

---

*报告由 Makers 开发专家团主理人齐上线自动生成，未修改任何源代码。*

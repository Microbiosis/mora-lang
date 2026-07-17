# Mora v0.25 开发工作流全记录

> **日期**: 2026-06-30
> **开发者**: Microbiosis + ZCode AI
> **目标**: V2 迁移 + 新特性实现 + Bug 修复 + 模块化拆分
> **面向**: 学习者——理解大型语言项目如何渐进式重构

---

## 目录

1. [总体概览](#1-总体概览)
2. [Phase 1: V2 AST 迁移](#2-phase-1-v2-ast-迁移)
3. [Phase 2: Bug 修复](#3-phase-2-bug-修复)
4. [Phase 3: 新特性实现](#4-phase-3-新特性实现)
5. [Phase 4: 模块化拆分](#5-phase-4-模块化拆分)
6. [关键设计决策](#6-关键设计决策)
7. [踩坑记录](#7-踩坑记录)
8. [数据统计](#8-数据统计)

---

## 1. 总体概览

### 1.1 背景

Mora 是一个 AI 原生脚本语言，用 Rust 实现。在 v0.24 时，代码库处于一个**过渡态**：

```
Source → ParserV2 → v2 AST → AstV2ToV1(转换层) → v1 AST → typeck(v1) → interpreter(v1)
```

ParserV2 已经产出 v2 AST，但下游的 typeck 和 interpreter 还停留在 v1，靠 `AstV2ToV1` 转换层衔接。这个架构有三个问题：

1. **性能损失**: 每次执行都要 v2→v1 转换
2. **维护负担**: 两套 AST 并存，改动需要同步
3. **死代码堆积**: 多个未使用的转换器

### 1.2 工作流总览

```
Phase 1: V2 迁移 (8 steps)
  ├── Step 1: 创建 common.rs 共享类型
  ├── Step 2: 删除死代码
  ├── Step 3: 切换生产管线到 v2
  ├── Step 4: 完善 execute_v2/evaluate_v2
  ├── Step 5: 完善 typeck check_stmt_v2
  ├── Step 6: 删除转换层
  ├── Step 7: 删除 v1 代码 + 重命名
  └── Step 8: 清理残留

Phase 2: Bug 修复 (36 项)
  ├── 一元负号、Match 表达式、字符串迭代
  ├── Partial 调用、Trait dispatch
  ├── Arena 借用、单表达式 body
  └── typeck 覆盖扩展

Phase 3: 新特性 (5 项)
  ├── Multi-Agent orchestrate (sequential/graph/loop)
  ├── Eval 原语 (断言 + tolerance)
  ├── Skill 原语 (能力包)
  ├── Memory 原语 (store/recall/search/save/load)
  └── Context Compaction (compact + Conversation.compact)

Phase 4: 模块化拆分 (7 个子模块)
  ├── ai_infra.rs (600 行)
  ├── interpreter/ai_chat.rs (826 行)
  ├── interpreter/ai_helpers.rs (368 行)
  ├── interpreter/builtins.rs (290 行)
  ├── interpreter/dispatch.rs (1047 行)
  ├── interpreter/orchestrate.rs (222 行)
  └── interpreter/trait_dispatch.rs (168 行)
```

### 1.3 最终成果

```
测试: 202/202 全部通过 ✅
clippy: ✅
代码量: 净减少 363 行（删除 3396 行旧代码，新增 3033 行新特性）
```

---

## 2. Phase 1: V2 AST 迁移

### 2.1 Step 1: 创建 common.rs

**动机**: v1 和 v2 AST 共享一些基础类型（Span, Literal, BinaryOp 等），需要提取到独立模块。

**操作**:
1. 创建 `src/common.rs`，包含 6 个共享类型
2. 更新 `ast_v2.rs` 导入路径：`use crate::common::*`
3. 更新 `parser_v2.rs` 导入路径
4. 更新 `ast.rs` 为 re-export + v1 专用类型

**关键代码**:
```rust
// common.rs
pub struct Span { pub line: usize, pub column: usize }
pub enum Literal { String(String, Span), Char(char, Span), Number(f64, Span), Bool(bool, Span), Nil(Span) }
pub enum BinaryOp { Add, Sub, Mul, Div, Mod, Equal, NotEqual, Greater, Less, GreaterEqual, LessEqual }
pub struct GenericParam { pub name: String, pub bound: Option<String>, pub span: Span }
pub struct EnumVariant { pub name: String, pub data: Option<String> }
pub struct StructField { pub name: String, pub type_hint: String }
```

**学习点**: 共享类型提取是模块化的第一步。关键是识别哪些类型被多个模块引用。

### 2.2 Step 2: 删除死代码

**操作**:
- 删除 `typed_ast.rs` (v1→v2 转换器，零调用)
- 删除 `ast_adapter.rs` (重复的 v1→v2 适配器，零调用)

**学习点**: 删除死代码前，用 `grep -rn` 确认零调用。这是最安全的重构步骤。

### 2.3 Step 3: 切换生产管线

**这是最关键的一步**——切断 v2→v1 的桥梁。

**操作**:
1. 删除 `ast_v2_to_v1.rs`
2. 重写 `main.rs` 入口：
```rust
// 之前: ParserV2 → AstV2ToV1 → v1 AST → typeck(v1) → interpret(v1)
// 之后: ParserV2 → v2 AST → typeck_v2(v2) → interpret_v2(v2)
fn run_file(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");
    let (node_ids, arena) = parse_with_v2(&source);
    let type_errors = typeck::check_program_v2(&node_ids, &arena);
    // ... error handling ...
    let mut interpreter = Interpreter::new();
    interpreter.interpret_v2(&node_ids, &arena)?;
}
```

**学习点**: 切换入口点时，可以先让新路径不完整（typeck_v2 只覆盖部分），后续再补全。渐进式迁移比一步到位更安全。

### 2.4 Step 4-5: 完善 v2 执行和类型检查

**操作**:
- `execute_v2`: 从 23/39 补全到 39/39 StmtKind 变体
- `check_stmt_v2`: 从 6/39 补全到 39/39 变体
- `check_expr_v2`: 从 6/21 扩展到 14/21 变体

**关键模式** (从 v1 移植到 v2):
```rust
// v1: Box<Expr> / Vec<Stmt>
fn check_expr(&mut self, expr: &Expr, symbols: &SymbolTable) -> Type { ... }

// v2: NodeId + arena 访问
fn check_expr_v2(&mut self, expr_id: NodeId, arena: &AstArena, symbols: &SymbolTable) -> Type {
    let expr = arena.get_expr(expr_id).unwrap();
    match &expr.kind {
        ExprKind::Literal(lit) => { ... }
        ExprKind::Binary { left, op, right } => {
            let left_ty = self.check_expr_v2(*left, arena, symbols);
            // ...
        }
    }
}
```

**学习点**: v1→v2 的核心变化是 `Box<Expr>` → `NodeId` + arena。逻辑不变，只是访问方式从直接解引用变为 `arena.get_expr(id)`。

### 2.5 Step 6-7: 清理 v1 代码

**操作**:
- 删除 `ast_v2_to_v1.rs` 转换层
- 删除 v1 `interpret()`, `execute()`, `evaluate()` 函数 (~1700 行)
- 重命名 `interpret_v2` → `interpret`, `execute_v2` → `execute`

**关键决策**: 保留 `match_pattern` 和 `evaluate_v1_expr`，因为 guard 条件是 v1 `Expr` 类型。

**学习点**: 删除大块代码时，先确认没有调用者。用 `grep -rn "function_name"` 搜索所有引用。

---

## 3. Phase 2: Bug 修复

### 3.1 一元负号 (Parser)

**问题**: `classify(-3)` 被解析为 `classify(- 3)`（二元减法）

**修复**: 在 parser 的 `unary()` 函数中添加一元负号支持：
```rust
fn unary(&mut self) -> NodeId {
    if self.check(&TokenType::Minus) {
        let span = self.span_of_current();
        self.advance();
        let operand = self.unary();
        let zero = self.arena.alloc_expr(
            ExprKind::Literal(Literal::Number(0.0, span)), span
        );
        let kind = ExprKind::Binary { left: zero, op: BinaryOp::Sub, right: operand };
        self.arena.alloc_expr(kind, span)
    } else { ... }
}
```

**学习点**: 一元负号 `-x` 可以简单地编译为 `0 - x`，避免引入新的 AST 节点。

### 3.2 Arena 借用死锁

**问题**: `memory.store("key", "val")` 调用挂起

**根因**: `self.environment` 和 `self.globals` 是同一个 `Arc<Mutex<Environment>>`，`evaluate_v2` 的 Call 处理中 `.or_else(|| globals.lock())` 导致死锁。

```rust
// 死锁代码
let func_val = self.environment.lock().expect("env").get(callee)
    .or_else(|| self.globals.lock().expect("globals").get(callee));  // 死锁！

// 修复：先 clone 释放锁
let func_val = self.environment.lock().expect("env").get(callee);  // 自动 clone
```

**学习点**: 当两个 Arc 指向同一个 Mutex 时，嵌套 lock 会死锁。解决方案是先 clone 值再释放锁。

### 3.3 Trait 默认实现 dispatch

**问题**: `test_trait_default_implementation_fallback` 失败——默认实现中的 `self.value()` 不返回值

**根因**: v2 TaskDef handler 注册 `v2_body_ids: vec![]`，导致默认实现 body 为空。

**修复**: 
```rust
// 之前: body 为空
v2_body_ids: vec![],

// 之后: 填充实际 body
let body_ids: Vec<usize> = body.iter().map(|id| id.0).collect();
v2_body_ids: body_ids,
```

**学习点**: v2 的 Task 和 Closure 不再持有 v1 `Vec<Stmt>` body，而是通过 `v2_body_ids` / `v2_node_id` 引用 arena。创建时必须填充这些 ID。

### 3.4 Dict 方法调用

**问题**: `Greeter.greet("World")` 报错 "Dict has no method: greet"

**根因**: Skill 的 task 注册在 Dict 内部，但 Dict 的方法调用分发不查找 Dict 内的值。

**修复**: 在 Dict 方法调用的 catch-all 中，检查 Dict 是否包含匹配的 callable 值：
```rust
// 之前
_ => Err(format!("Dict has no method: {}", method)),

// 之后
_ => {
    if let Some(val) = map.get(method) {
        match val {
            Value::Task { .. } | Value::Closure { .. } => {
                return self.call_value(val, args);
            }
            _ => {
                // 非 callable 值直接返回（如 metadata 字段）
                if args.is_empty() { return Ok(val.clone()); }
            }
        }
    }
    Err(format!("Dict has no method: {}", method))
}
```

**学习点**: 方法调用分发需要考虑对象类型的特殊语义。Dict 可以同时存储数据和可调用值。

---

## 4. Phase 3: 新特性实现

### 4.1 Multi-Agent orchestrate

**设计文档**: `docs/multi-agent-design.md`

**语法**:
```mora
-- Sequential: 线性管道
orchestrate sequential input -> result
  agent a task(ai.chat(p"Step 1: {input}")) end
  agent b task(ai.chat(p"Step 2: {input}")) end
end

-- Graph: 有向图 + 条件路由
orchestrate graph input -> result
  agent a task(...) end
  agent b task(...) end
  edges
    @start -> a
    a -> b when rounds < 2
    b -> @exit
  end
end

-- Loop: 迭代精炼
orchestrate loop input -> result, max_rounds: 3
  agent improver task(ai.chat(p"Improve: {input}")) end
  exit_when: result.contains("done")
end
```

**AST 定义**:
```rust
pub enum OrchestrateKind {
    Sequential { agents: Vec<OrchestrateAgent> },
    Graph { agents: Vec<OrchestrateAgent>, edges: Vec<OrchestrateEdge> },
    Loop { agent: OrchestrateAgent, max_rounds: usize, exit_when: Option<NodeId> },
}

pub struct OrchestrateAgent {
    pub name: String,
    pub with_config: Option<Vec<(String, NodeId)>>,
    pub task_expr: NodeId,
    pub verify_expr: Option<NodeId>,
}

pub struct OrchestrateEdge {
    pub from: String,
    pub to: String,
    pub condition: Option<NodeId>,
}
```

**执行器核心逻辑** (Graph 模式):
```rust
fn execute_orchestrate(&mut self, ...) -> Result<FlowSignal, String> {
    match kind {
        OrchestrateKind::Graph { agents, edges } => {
            let mut current = input;
            let mut current_node = "@start".to_string();
            let mut rounds_map: HashMap<(String, String), usize> = HashMap::new();

            loop {
                // 找匹配的边
                let next_edge = edges.iter().find(|e| {
                    e.from == current_node && match &e.condition {
                        Some(cond_id) => self.evaluate(*cond_id, arena)
                            .map(|v| matches!(v, Value::Bool(true)))
                            .unwrap_or(false),
                        None => true,
                    }
                });

                match next_edge {
                    None | Some(OrchestrateEdge { to: "@exit", .. }) => break,
                    Some(edge) => {
                        let agent = agents.iter().find(|a| a.name == edge.to).unwrap();
                        *rounds_map.entry((edge.from.clone(), edge.to.clone())).or_insert(0) += 1;
                        current = self.run_orchestrate_agent(agent, &current, arena)?;
                        current_node = edge.to.clone();
                    }
                }
            }
        }
    }
}
```

### 4.2 Eval 原语

**语法**:
```mora
eval "代码审查质量"
  given: sample_code
  expect: result.contains("error")
  expect: result.len() > 50
  tolerance: 0.8
  replay: "recordings/test.jsonl"
end
```

**关键设计决策**:
- `given` 是上下文关键字（不是全局关键字），避免阻塞表达式中的使用
- `tolerance` 支持 LLM 非确定性容忍度
- `replay` 集成现有 record/replay 系统

### 4.3 Skill 原语

**语法**:
```mora
skill CodeReviewer
  description: "审查代码质量"
  version: "1.0.0"
  requires: [git, diff]

  task review(code: string): string
    return ai.chat(p"审查以下代码：\n{code}")
  end

  task summarize(review: string): string
    return ai.chat(p"总结：\n{review}")
  end

  verify(result: string)
    return result.len() > 0
  end
end
```

**运行时语义**: Skill 编译为 Dict，task 存储在 Dict 内部：
```mora
-- 等价于:
let CodeReviewer = {
  "name": "CodeReviewer",
  "description": "审查代码质量",
  "version": "1.0.0",
  "requires": ["git", "diff"],
  "review": Task { name: "review", params: ["code"], v2_body_ids: [...] },
  "summarize": Task { ... },
  "verify": Task { ... }
}
```

**调用**: `CodeReviewer.review("code")` 通过 Dict 方法调用分发，查找 Dict 内的 `review` 值。

### 4.4 Memory 原语

**API**:
```mora
memory.store("key", value)    -- 存储
memory.recall("key")          -- 精确查找
memory.search("query")        -- 模糊搜索（key 包含匹配）
memory.forget("key")          -- 删除
memory.clear()                -- 清空
memory.save("./data.json")    -- JSON 持久化
memory.load("./data.json")    -- 从文件加载
memory.size()                 -- 条目数
memory.keys()                 -- 所有键
```

**实现**: `HashMap<String, Value>` + JSON 持久化

### 4.5 Context Compaction

**API**:
```mora
-- 文本摘要
let summary = compact(long_text)

-- 对话压缩
let conv = ai.new_conversation("gpt-4")
conv.chat("...")
let summary = conv.compact()

-- 自动压缩阈值
with model("gpt-4"), compact_at(80) do
  -- token 用量达到 80% 时自动压缩
end
```

**关键修复**: builtin 对象（ai/web/json/file/memory/agent）需要在 `Interpreter::new()` 中注册到环境：
```rust
for name in &["ai", "web", "json", "file", "memory", "agent"] {
    globals.lock().unwrap()
        .define(name.to_string(), Value::Builtin(name.to_string()), false);
}
```

---

## 5. Phase 4: 模块化拆分

### 5.1 拆分策略

将 `interpreter.rs`（7286 行）按功能拆分为多个子模块：

```
之前: interpreter.rs (7286 行)
之后: interpreter/
├── mod.rs            (3402 行 — 核心)
├── ai_chat.rs        (826 行)
├── ai_helpers.rs     (368 行)
├── builtins.rs       (290 行)
├── dispatch.rs       (1047 行)
├── orchestrate.rs    (222 行)
└── trait_dispatch.rs (168 行)
```

### 5.2 Rust 目录模块模式

```rust
// src/interpreter/mod.rs
mod ai_chat;
mod ai_helpers;
mod builtins;
mod dispatch;
mod orchestrate;
mod trait_dispatch;

use crate::ai_infra::*;  // 外部模块
use crate::flow::*;
// ... 核心 Interpreter 结构体和方法 ...
```

```rust
// src/interpreter/dispatch.rs
use super::*;  // 访问父模块的所有内容
use crate::common::Span;
use crate::value::Value;

impl Interpreter {
    pub(super) fn call_function(...) -> Result<Value, String> { ... }
    pub(super) fn call_method(...) -> Result<Value, String> { ... }
    pub(crate) fn call_value(...) -> Result<Value, String> { ... }
}
```

**学习点**:
- Rust 允许同名结构体在多个文件中有 `impl` 块
- `pub(super)` 限制可见性在父模块内
- `pub(crate)` 限制可见性在整个 crate 内
- 子模块通过 `use super::*` 访问父模块内容

### 5.3 可见性陷阱

**问题**: `call_value` 被 `ai_chat.rs` 和 `builtins.rs` 调用，但初始设为 `pub(super)` 导致其他子模块无法访问。

**解决**: 核心方法用 `pub(crate)`，辅助方法用 `pub(super)`。

### 5.4 拆分顺序

1. 先提取独立的结构体（`ai_infra.rs`）— 最安全
2. 再提取纯函数（`ai_helpers.rs`）— 无状态依赖
3. 然后提取方法组（`dispatch.rs`, `builtins.rs`）— 需要 `impl` 块
4. 最后提取耦合紧密的方法（`ai_chat.rs`, `trait_dispatch.rs`）— 需要仔细处理依赖

---

## 6. 关键设计决策

| 决策 | 理由 |
|------|------|
| 渐进式迁移（非一步到位） | 每步可验证，出错可回滚 |
| 共享类型提取到 common.rs | 减少 v1/v2 之间的耦合 |
| 保留 match_pattern 的 v1 依赖 | guard 条件是 v1 Expr，改成本高 |
| orchestrate 用 `orchestrate` 关键字 | 与 parallel/transaction 一致，编译器可优化 |
| Skill 编译为 Dict | 复用现有 Dict 方法调用机制 |
| Memory 用 HashMap | 简单高效，JSON 持久化 |
| builtin 对象需显式注册 | environment.get() 只查找已注册的值 |
| `pub(super)` vs `pub(crate)` | 核心方法 `pub(crate)`，辅助方法 `pub(super)` |

---

## 7. 踩坑记录

### 7.1 双锁死锁

**场景**: `self.environment` 和 `self.globals` 是同一个 `Arc<Mutex<Environment>>`

```rust
// ❌ 死锁
let val = self.environment.lock().get(key)
    .or_else(|| self.globals.lock().get(key));  // 同一个 Mutex!

// ✅ 修复：先 clone 释放锁
let val = self.environment.lock().get(key).cloned();
```

### 7.2 sed 过度删除

**场景**: 用 `sed -i '100,200d'` 删除函数，但行号包含闭合大括号导致 impl 块未闭合。

**教训**: 删除前先用 `grep -n "^    }$"` 确认函数边界。更安全的做法是逐个函数删除，每次验证编译。

### 7.3 关键字冲突

**场景**: `given`、`description`、`version` 等作为全局关键字，导致 `Greeter.description` 无法解析。

**解决**: 改为上下文关键字（只在特定块内识别为关键字），其他地方作为普通标识符。

### 7.4 Arena 借用 vs 所有权

**场景**: `call_value` 需要 arena 来执行 v2 闭包，但 arena 在 `interpret_v2` 中以 `&AstArena` 传入。

**解决**: 在 Interpreter 上存储 `v2_arena: Option<AstArena>`，`interpret_v2` 入口时 clone 存储，`call_value` 时借用。

### 7.5 Builtin 对象未注册

**场景**: `memory.store("key", "val")` 报 "Undefined variable: memory"

**根因**: `memory` 在 `is_builtin_object()` 中列出，但未在 `Interpreter::new()` 中注册到环境。

**修复**: 在 `new()` 中显式注册所有 builtin 模块对象。

---

## 8. 数据统计

### 8.1 代码量变化

| 文件 | 之前 | 之后 | 变化 |
|------|------|------|------|
| interpreter.rs | 7286 | 3402 | **-3884** |
| typeck.rs | ~4400 | 2838 | **-1562** |
| ast.rs | 439 | 0 | **-439** |
| ast_v2_to_v1.rs | 503 | 0 | **-503** |
| typed_ast.rs | 605 | 0 | **-605** |
| ast_adapter.rs | 588 | 0 | **-588** |
| 新增 common.rs | 0 | 73 | +73 |
| 新增 ai_infra.rs | 0 | 600 | +600 |
| 新增 interpreter/*.rs | 0 | 2921 | +2921 |
| **净变化** | | | **-3387** |

### 8.2 测试变化

| 阶段 | 通过 | 失败 |
|------|------|------|
| 迁移前 | 188 | 0 |
| Step 1-6 | 150 | 38 |
| Step 7 | 148 | 40 |
| Bug 修复后 | 202 | 0 |

### 8.3 提交历史

```
1af52fd refactor: 模块化拆分 interpreter — AI 辅助函数 → ai_helpers.rs
1afc336 refactor: 模块化拆分 interpreter — 函数分发 → dispatch.rs
5bb93b3 refactor: 模块化拆分 interpreter — Trait 分发 → trait_dispatch.rs
7413182 refactor: 模块化拆分 interpreter — AI 聊天 → ai_chat.rs
4e05abd refactor: 模块化拆分 interpreter — 内置函数 → builtins.rs
aa24fb6 refactor: 拆分 interpreter.rs — AI 基础设施 → ai_infra.rs
e5d10a7 refactor: 模块化拆分 interpreter — 目录结构 + orchestrate 子模块
6c42ded feat: Memory + Context Compaction 原语 + builtin 注册修复
ec0a671 feat: Eval replay 集成 + 示例文件
b058095 feat: 实现 Eval + Skill 原语
f2be108 feat: 实现 Multi-Agent orchestrate 协调模式
ba90114 fix: 修复全部技术债 — 188/188 测试通过
143e53f fix: 大幅完善 typeck check_expr_v2
d716bf7 fix: 修复多个 bug — for/string、guard、trait dispatch、partial
4e5ca15 fix: 恢复 match_pattern guard 条件完整求值逻辑
d919c1e refactor: 移除 Value::Task/Closure/Macro 的 v1 body 字段
463d3a7 refactor: 删除 v1 execute/evaluate + 辅助函数
1704df5 refactor: 删除 v1 interpret + 添加 v2 task 执行
8c30780 refactor: 删除 ast_v2_to_v1.rs
e70a9b6 refactor: 完善 typeck check_stmt_v2
2b1e30b refactor: 完善 execute_v2 + evaluate_v2
313ac12 refactor: 切换生产管线到 v2
915857a refactor: 共享类型提取 + 删除死代码
```

---

## 附录: 学习者指南

### A. 渐进式重构的黄金法则

1. **每步可编译、可测试** — 永远不要一次改太多
2. **先加后删** — 新路径先跑通，再删旧路径
3. **先死代码后活代码** — 先删零调用的代码，再删有调用的
4. **先简单后复杂** — 先提取独立模块，再提取耦合模块
5. **每步 commit** — 出错可 `git revert`

### B. Rust 模块化技巧

```rust
// 目录模块: src/interpreter/mod.rs + src/interpreter/*.rs
mod ai_chat;       // 子模块声明
mod builtins;

// 子模块访问父模块
use super::*;      // 导入父模块所有 pub 内容

// 可见性控制
pub(super) fn helper() {}   // 仅父模块可见
pub(crate) fn public() {}   // 整个 crate 可见

// 同一结构体多文件 impl
// src/interpreter/dispatch.rs
impl Interpreter {
    pub(super) fn call_function(...) { ... }
}
```

### C. 渐进式迁移清单

- [ ] 识别当前架构的过渡态
- [ ] 列出所有需要迁移的组件
- [ ] 按依赖关系排序（先底层后上层）
- [ ] 每步创建新路径，验证通过后再删旧路径
- [ ] 保持测试绿色（188/188 或更好）
- [ ] 每步 commit，方便回滚

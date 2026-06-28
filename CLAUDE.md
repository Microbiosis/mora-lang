# CLAUDE.md — Claude Code 工作约定（mora-lang）

详细规则见同目录 [`AGENTS.md`](./AGENTS.md)。本文件仅放 Claude Code 特有的提示。

## 搜索优先规则（必读）

> **任何外部知识都先走 MCP 搜索，不要直接用训练数据回答。**

具体工作流、可触发场景、可用工具见 `AGENTS.md` 第 1 节。摘要：

- 触发：第三方库 API/版本、协议规范、最新动态、最佳实践 → 必搜
- 不触发：Mora 内部语法/语义、仓库内部代码 → 直接读 `src/`、`docs/`
- 工具：`mcp__anysearch__search` / `batch_search` / `extract` / `get_sub_domains`
- 原则：先搜后答；搜索结果与训练数据冲突时**以搜索为准**；关键结论附来源 URL

## 其它提醒

- 本仓库是 Mora 语言实现（Rust），当前版本 **v0.23**
- 项目阶段是 **v0.x 永远逼近 v1.0**:
  - v1.0 是真实方向(形式化语义、HM 推断、SemVer 稳定性等),不是兑诺
  - 这些方向的工作**持续在做**,从 v0.13 起每个版本都推进一部分
  - 因为 v1.0 不到达,所以这些工作**永远做不完,永远在优化**
  - 永远不做的事 = 写"v1.0 推迟到 v1.0"的装腔话术 = **不要这么做**
- 改 Rust 依赖前先用 `mcp__anysearch__search` 确认目标 crate 的最新稳定版与 breaking change。
- 写代码前先 `grep` 仓库内已有实现。

## 代码结构

- `src/value.rs` — Value/Environment/FlowSignal 核心类型
- `src/flow.rs` — 自由函数 + JSON 解析
- `src/interpreter.rs` — 解释器核心
- `src/ast.rs` — AST 定义
- `src/parser.rs` — 解析器
- `src/lexer.rs` — 词法分析
- `src/typeck.rs` — 类型检查

## 语言设计参考

- **语言规范**: `docs/mora-spec.md` (20 章)
- **影响分析**: `docs/influences.md` (9 语言)
- **学习计划**: `docs/learning-plan.md` (6 阶段)
- **变更日志**: `CHANGELOG.md`
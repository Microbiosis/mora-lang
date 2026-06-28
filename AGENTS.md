# Agent Rules for mora-lang

本文件约束所有 AI 编码助手在此仓库中的工作方式。Claude Code、Cursor、Aider、Copilot 等工具都应遵守。

## 1. 信息来源：默认走 MCP 搜索，训练数据仅作辅助

> **核心规则**：凡是涉及**外部知识**的信息，必须通过 MCP 搜索工具获取，不要仅依赖训练数据。

### 1.1 适用范围

以下场景**必须**先调用 MCP 搜索：

- 任何第三方库的 API、配置、版本兼容性（如 `ureq`、`serde`、`tokio`、`rmcp` 等）
- 任何外部协议/规范的最新动态（如 MCP 协议、HTTP 标准、语言标准库）
- 当前日期附近的发布说明、breaking change、CVE
- 官方推荐的 best practice、迁移指南
- 用户给出的链接之外、需要补全的"周边"知识

### 1.2 例外（可直接用训练数据）

- Mora 语言自身的语法、语义、AST 形状 —— 直接读 `src/` 与 `docs/`
- 项目内部命名、模块结构、历史 commit 含义 —— 直接 `grep` / `git log`
- 通用编程语言基础（Rust 语法、Python 语法等）

### 1.3 可用工具

本仓库默认使用 **`mcp__anysearch__*` 系列** 作为 MCP 搜索入口：

- `mcp__anysearch__search` — 单次搜索，可指定领域（`general` / `code` / `academic` 等）
- `mcp__anysearch__batch_search` — 多 query 并发搜索
- `mcp__anysearch__extract` — 抓取指定 URL 内容
- `mcp__anysearch__get_sub_domains` — 查询垂直子域枚举

涉及代码/API 调研时优先 `domain="code"`；通用事实用默认。

### 1.4 工作流

1. **先识别**：判断问题是否需要"外部最新事实"。是 → 进入第 2 步；否 → 直接答/读代码。
2. **先搜后答**：在写出结论、代码、命令前，至少完成一次 MCP 搜索；如搜索结果与训练数据冲突，**以搜索为准**，并在答复里标注来源。
3. **不确定就再搜**：对版本号、参数名、错误码、API 形态拿不准时，宁可多搜一次，不要凭印象写。
4. **结果引用**：在最终答复里给出关键结论对应的来源 URL，便于人工复核。

## 2. 其它通用约定

- 改代码前先 `grep` 现有实现，避免重复造轮子。
- 涉及 Rust 依赖变更，优先搜索 crate 最新稳定版本和迁移说明，再改 `Cargo.toml`。
- 提交信息遵循仓库现有风格（参考近期 `git log`）。

## 3. 代码质量

- **clippy**: 所有代码必须通过 `cargo clippy --all-targets --all-features -- -D warnings`
- **测试**: 新功能必须有对应测试，测试数量只增不减
- **unwrap**: 生产代码中避免 `unwrap()`，使用 `expect("有意义的错误信息")`
- **模块化**: 大文件应拆分为多个模块 (value.rs, flow.rs 等)

## 4. 语言设计参考

- **语言规范**: `docs/mora-spec.md` (20 章)
- **影响分析**: `docs/influences.md` (9 语言)
- **学习计划**: `docs/learning-plan.md` (6 阶段)
- **变更日志**: `CHANGELOG.md`

## 5. 版本管理

- 版本号遵循语义化版本 (Semantic Versioning)
- v0.x 阶段可能有 breaking change
- 每个版本的变更必须记录在 CHANGELOG.md
# Changelog

All notable changes to Mora will be documented in this file.

## [v0.26] - 2026-07-01

### Prompt Sections — 分段 + 容量预算 + 滚动窗口

灵感来自 [mimiclaw](https://github.com/memovai/mimiclaw)（5 段固定缓冲）和 [headroom](https://github.com/headroomlabs-ai/headroom)（内容感知路由器），把 LLM 的 system prompt 拼装从字符串拼接升级为分段工程。

#### 新增关键字 `prompt`

```mora
prompt "identity" do
    set role: "system"
    set budget: "256 B"
    read "./SOUL.md"
end

prompt "memory" do
    set role: "system"
    set budget: "8 KB"
    tail("./sessions/today.jsonl", max: 20)
end

let sys = compose_prompt("identity", "memory")
```

#### 新增内建函数

| 名字 | 作用 |
|---|---|
| `compose_prompt(...)` | 拼接多段为单一 system prompt，按 section budget 截断 |
| `tail(path, max: N)` | 取文件末 N 行（JSONL/纯文本） |

#### 新增值类型

- `Value::PromptSection { name, role, text, budget_bytes }`

#### 新增 AST 节点

- `StmtKind::PromptSection { name, body }`
- `StmtKind::PromptSet { key, value }`（块内 `set role:` / `set budget:`）
- `StmtKind::PromptRead(NodeId)`（块内 `read`）

#### 技术细节

- **零依赖**：无 tokenizer，按 UTF-8 字节近似（与 mimiclaw 同思路）
- **可逆性**：每个 section 在环境里是可读 Value，便于调试与中间表示（IR）思路）
- **可组合**：字典内联形参与块式声明产生同义结果

## [v0.25] - 2026-07-01

### 代码模块化重构 (Code Modularization)

对 5 个大文件进行了模块化拆分，提升代码可维护性：

#### 拆分详情
- **interpreter**: 3402 行 → 3 文件 (mod.rs + execute.rs + evaluate.rs)
- **typeck**: 2838 行 → 2 文件 (mod.rs + check.rs)
- **parser_v2**: 2609 行 → 3 文件 (mod.rs + statements.rs + expressions.rs)
- **record**: 2091 行 → 7 文件 (mod.rs + serialization.rs + diff.rs + analysis.rs + audit.rs + snapshot.rs + tests.rs)
- **lsp/providers**: 1092 行 → 11 文件 (mod.rs + helpers.rs + 9 个 provider 模块)

#### 改进
- 每个模块职责单一，便于理解和维护
- 函数按功能分组，提高代码可读性
- 模块间依赖关系更清晰

### 跨平台兼容性修复
- 修复 `test_memory_save_load` 测试在 Windows 上的路径问题
- 使用 `std::env::temp_dir()` 替代硬编码的 `/tmp` 路径

## [v0.24] - 2026-06-30

### ParserV2 完整迁移 (Complete)

ParserV2 已完成对旧 Parser 的完整迁移，所有功能已覆盖。
旧 parser.rs (2459 行) 已删除，主程序和测试全部使用 ParserV2。

#### 新增语句解析
- **append_statement**: 追加文件写入
- **read_bytes_statement**: 读取字节文件
- **write_bytes_statement**: 写入字节文件
- **stream_statement**: 流式循环 `stream <expr> as <var> do ... end`
- **tool_statement**: 工具定义 `tool name(params): type do ... end`
- **observe_statement**: 可观测性配置 (trace/metrics/otel)
- **span_statement**: 追踪范围 `span "name" tags {..} do ... end`
- **record_tokens_statement**: 记录 token 使用量
- **assignment_statement**: 赋值语句 `IDENT = expr`
- **index_assignment**: 索引赋值 `IDENT[expr] = expr`
- **commit/rollback**: 事务提交/回滚

#### 新增表达式解析
- **match_expression**: 模式匹配表达式 (含 when 守卫)
- **pattern**: 模式解析 (字面量/变量/列表/字典/通配符)
- **parse_format_string**: 格式字符串插值
- **parse_ai_model_call**: ai_model 调用 (支持 keyword args)
- **flatten_prompt_parts**: Prompt 表达式展平
- **list_literal / dict_literal**: 列表和字典字面量
- **char_literal**: 字符字面量 `'a'`
- **NamespaceRef**: 命名空间引用 `Module::method()`

#### 新增类型系统支持
- **parse_generic_params**: 泛型参数 `<T: Bound>`
- **parse_type_list**: 类型列表 `<T, U, V>`
- **parse_type_name_recursive**: 递归解析嵌套泛型
- **parse_where_clause**: where 子句

#### 类型检查修复
- **let 推断**: 已知类型自动推断，不再强制要求类型注解
- **string + any**: 允许字符串拼接 (运行时做类型转换)

#### 重构
- **ObserveConfig**: 在 ast_v2.rs 中定义新类型，使用 NodeId
- **FnDef / TraitMethod**: 在 ast_v2.rs 中定义新类型，使用 Vec<NodeId>
- **Pattern**: 在 ast_v2.rs 中定义新类型，Guard condition 使用 NodeId
- **consume_method_name**: 支持关键字作为方法名
- **表达式优先级**: 修复方法调用优先级 (binary → unary → call → primary)
- **反向适配器**: ast_v2_to_v1.rs 支持完整 AST 转换

### 9 Languages Features Integration (Complete)

All features from the learning plan have been implemented.

### v0.21: Rust 风格类型系统

- **借用语法**: `&expr` / `&mut expr`
- **生命周期标注**: `<'a>` 参数
- **借用冲突检查**: 编译期检查不可变/可变借用冲突

### v0.22: 性能优化

- **AI 调用内联缓存**: 相同 prompt 直接返回缓存结果
- **管道融合**: 连续 map/filter/take/drop 合并执行
- **常量折叠**: 编译期计算常量表达式
- **字符串驻留**: 相同字符串只存储一次
- **HTTP 连接池**: 线程池优化 (最多16线程)
- **MCP 异步处理**: 线程池处理请求 (最多8并发)
- **类型检查增量优化**: 缓存已检查的表达式类型

### v0.24: 强类型升级

- **类型别名**: `type Name = TargetType`
- **枚举类型**: `enum Name { V1, V2(Type) }`
- **结构体类型**: `struct Name { field: Type }`

### 文档

- **docs/mora-spec.md**: Mora 语言规范 (20 章)
- **docs/influences.md**: 9 语言影响分析
- **docs/learning-plan.md**: 特性融入计划
- **docs/workflow-v0.20.md**: 开发工作流

From Prolog, StreamIt, APL, Clojure, Lisp, Smalltalk, Common Lisp, Ballerina, Logo.

#### Pattern Matching Enhancement (Prolog)
- **Match guard conditions**: `match n with x when x > 0 -> ... end`
- **List rest pattern**: `[head, ...tail] = [1, 2, 3]`
- **Dict partial match**: `{name: n} = {"name": "Alice", "age": 30}`

#### Pipe & Stream (StreamIt + APL)
- **Pipe with closure**: `5 |> fn(x) return x * 2 end`
- **Window aggregation**: `[1,2,3,4,5].window(3)` → `[[1,2,3],[2,3,4],[3,4,5]]`
- **Batch processing**: `[1,2,3,4,5].batch(3)` → `[[1,2,3],[4,5,6],[7]]`
- **Array operations**: `.shape()`, `.flatten()`, `.transpose()`, `.reshape()`
- **Broadcast arithmetic**: `[1,2,3] * 2` → `[2,4,6]`

#### Functional Core (Clojure + Lisp)
- **Compose**: `compose(f, g, h)` → composed function
- **Take/Drop**: `[1,2,3].take(2)` → `[1,2]`, `[1,2,3].drop(1)` → `[2,3]`
- **Partial application**: `partial(add, 10)` → partial applied function

#### Concurrency (Clojure)
- **Atom**: `atom(0)` → mutable reference
- **Swap**: `swap(counter, fn(n) return n + 1 end)`
- **Deref**: `deref(counter)` → current value

#### Reflection (Smalltalk)
- **type_of**: `type_of(42)` → `"number"`
- **is_instance**: `is_instance("hello", "string")` → `true`
- **methods_of**: `methods_of([1,2])` → `["push","pop","map",...]`
- **Message chain**: Router methods return self for chaining

### Statistics
- **Tests**: 147 → 178 (+31)
- **Code**: +7010 / -1517 lines

## [v0.15] - 2026-06-28

### AI Config Integration

- **TokenBudget.per_call**: Per-call token limit check
- **real_ai_chat_with_tools**: Now reads temperature/max_tokens/system from config
- **Route config**: RouteConfig settings now applied to AI calls
- **with mock_llm**: Mock LLM response queue for testing

### Record CLI Extension

- **mora record list**: List all recordings
- **mora record stats**: Show recording statistics
- **mora record timeline**: Show call timeline
- **mora record export**: Export JSONL/Markdown
- **mora record audit**: Secret scanning with .moraignore
- **mora record report**: Evidence report generation
- **mora snapshot**: Snapshot testing for regression

### Documentation

- **docs/mora-spec.md**: Mora Language Specification (20 chapters)
- **docs/influences.md**: 9 Languages Influence Analysis
- **docs/learning-plan.md**: Feature Integration Plan

### Statistics
- **Tests**: 126 → 147 (+21)

## [v0.14] - 2026-06-27

### Record/Replay/Diff CLI

- **mora record**: Record AI calls to JSONL
- **mora replay**: Replay recordings deterministically
- **mora diff**: Compare two recordings

### Statistics
- **Tests**: 121 → 126 (+5)

## [v0.13] - 2026-06-26

### Breaking Changes

- Removed `Type::Any` variant
- Removed Walrus syntax (`:=`)

### Statistics
- **Tests**: 113 → 121 (+8)

---

## Version History

| Version | Date | Tests | Key Features |
|---------|------|-------|--------------|
| v0.20 | 2026-06-28 | 178 | 9 languages integration |
| v0.15 | 2026-06-28 | 147 | AI config + record CLI |
| v0.14 | 2026-06-27 | 126 | record/replay/diff |
| v0.13 | 2026-06-26 | 121 | Remove Type::Any |

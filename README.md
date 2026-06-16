# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)

一门 AI 原生的轻量级脚本语言。AI 不是库，是语言的一等公民。

```mora
-- 流式输出
for token in ai.stream("写一首诗")
  print(token)
end

-- 工具调用（AI 自动决定调用哪个工具）
ai.tool("search", "搜索", "{...}", fn(args) return "结果" end)
let answer = ai.chat("帮我搜索")

-- 长期记忆
memory.store("项目", "Mora 是 AI 原生脚本语言")
let ctx = memory.recall("编程语言", 3)

-- Agent 编排
let agent = agent.create("researcher", {"tools": ["search"], "max_steps": 10})
let report = agent.run("调研 AI 语言趋势")

-- Token 预算
ai.budget({total: 100000})
let usage = ai.usage()
```

## 安装

从 [Releases](https://github.com/Microbiosis/mora-lang/releases/tag/v0.03) 下载对应平台的二进制包（包含 `mora` + `mora-lsp`），解压后即可使用。

| 平台 | 文件 |
|------|------|
| Windows x86_64 | `mora-x86_64-pc-windows-msvc.zip` |
| Linux x86_64 (glibc) | `mora-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (musl) | `mora-x86_64-unknown-linux-musl.tar.gz` |
| macOS Intel | `mora-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `mora-aarch64-apple-darwin.tar.gz` |

可选：将 `mora` 所在目录加入系统 PATH，即可在任意位置运行 `mora` 命令。

## 使用

```bash
# 运行脚本
mora script.mora

# 交互式 REPL
mora --repl

# 仅类型检查（不运行）
mora --check script.mora

# 启动 LSP 语言服务（供编辑器调用）
mora-lsp

# 从源码编译（需要 Rust 环境）
cargo build --release
```

## 语言特性

### 基础语法

| 特性 | 语法 |
|------|------|
| 变量 | `let x = 1`，`let s: string = "你好"` |
| 函数 | `task foo(x: string): string ... end` |
| 闭包 | `fn(x) x + 1 end` |
| 列表 | `[1, 2, 3]`，`list.map(fn)`，`list.filter(fn)`，`list.reduce(fn, init)` |
| 字典 | `{key: val}`，`dict.get("key")` |
| 字符串 | `.len()`、`.upper()`、`.lower()`、`.trim()`、`.split()`、`.contains()`、`.replace()` |
| 流程控制 | `if/then/end`、`for x in list/end`、`try/catch/end`、`match expr with/end` |
| 管道运算符 | `data \|> func()` |
| 并行执行 | `parallel ... end` |
| 模块系统 | `import "path"`，`export let/task` |

### 标准库

| 模块 | 功能 |
|------|------|
| `json.*` | `json.parse(text)`，`json.stringify(value)` |
| `web.*` | `web.fetch(url)`（真实 HTTP，基于 ureq） |
| `file.*` | `read_text/write_text/append_text/read_bytes/write_bytes`、`exists/is_file/is_dir/size/list/mkdir/remove/rename/copy/touch`、`cwd/chdir/home_dir/join/abs/basename/dirname/extname` |
| 持久化 | `save "file.json", value`，`load "file.json", var` |
| 文件语法糖 | `read "a.txt" into x`，`write "a.txt", content`，`append "a.txt", content` |

### AI 原生特性（v0.03）

| 特性 | 语法 | 说明 |
|------|------|------|
| AI 对话 | `ai.chat(prompt)` | 兼容 OpenAI API，支持 mock 模式 |
| 多轮对话 | `ai.create(model)` → `conv.chat(prompt)` | 带历史记忆 |
| 流式输出 | `for token in ai.stream(prompt)` | SSE 流式，逐 token 迭代 |
| 工具调用 | `ai.tool(name, desc, schema, fn)` | Function Calling，自动循环 |
| 向量嵌入 | `ai.embed(text \| list)` | 真实 OpenAI embeddings |
| 向量运算 | `ai.cosine/dot/euclidean/norm` | 向量相似度计算 |
| 语义检索 | `ai.search(query, corpus, k?)` | 无 key 走 mock 词袋 |
| 多模型路由 | `ai.route({"fast": "gpt-4o-mini"})` | 按任务选模型 |
| Token 预算 | `ai.budget({total})` / `ai.usage()` | 消耗追踪 + 告警 |
| 长期记忆 | `memory.store/recall/forget/clear/list/len` | 向量存储 + 语义检索 |
| Agent 编排 | `agent.create(name, config).run(task)` | 多步推理循环 |
| 输出评估 | `agent.critic(text)` / `agent.critic(text, ctx)` | 质量评估 + 幻觉检测 |

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用真实 AI 调用 | （空 = mock 模式） |
| `MORA_AI_MODEL` | AI 模型名称 | `gpt-4o-mini` |
| `MORA_AI_BASE_URL` | API 端点地址 | `https://api.openai.com/v1` |
| `MORA_EMBED_MODEL` | Embedding 模型 | `text-embedding-3-small` |
| `MORA_NO_TYPECK` | 设为 `1` 跳过静态类型检查 | （空 = 启用） |

## v0.04 终态: 云服务原生 (Single-binary Multi-protocol)

v0.04 终态把 HTTP server / MCP server / 可观测全部下沉到语言层。一段 Mora 脚本可以同时起 HTTP + MCP server + trace, 单进程, 零外部依赖。

### `serve` 块 (单二进制多协议)

```mora
-- 一段代码 = HTTP server + MCP server + 可观测
observe trace

route fast: "gpt-4o-mini"
route deep: "gpt-4o"

serve as http on port 3000 do
  GET "/health" -> fn(req) return {status: "ok"} end
  POST "/chat"  -> fn(req) return deep(p"用 deep 回答: {req.body.text}") end
end

serve as mcp do
  tool search(query: string): string do
    return "found: " + args["query"]
  end
end
```

跑 `mora script.mora` —— 单进程同时起 HTTP (3000) + MCP (stdio) + trace。

| 协议 | 关键字 | 用途 |
|------|--------|------|
| HTTP | `serve as http on port N` | REST API (动态路由) |
| MCP | `serve as mcp` | Claude Desktop 等 MCP 客户端 |
| REPL | `serve as repl` | (v0.04.1) |
| Stdio | `serve as stdio` | (v0.04.1) |

### `route` 块 (模型绑定)

```mora
route fast: "gpt-4o-mini"
route deep: "gpt-4o"

let s = fast(p"summarize: {text}")
let a = deep(p"analyze: {question}")
```

`fast(p"...")` 自动绑到 `gpt-4o-mini`, `deep(p"...")` 绑到 `gpt-4o`。

### `observe` / `span` 块 (可观测)

```mora
observe trace
observe otel endpoint "http://otel-collector:4317"

span "user_request" tags {user_id: u.id} do
  let r = deep(p"...")
  record_tokens(input, output)
end
```

`observe` 启用 trace, `span` RAII 风格的子块。`record_tokens` 显式计 token。

## 静态类型检查

Mora v11 在解释执行前自动做一遍轻量静态类型检查。

```mora
let name: string = "mora"          -- ✅ 匹配
let age: number = "thirty"         -- ❌ typeck: string → number
task add(a: number, b: number): number
  return a + b
end
add(1, 2)                          -- ✅
add(1)                             -- ❌ 期望 2 个参数
add("x", 2)                        -- ❌ 参数 1 期望 number
```

**规则**：
- 多错误一次报告（不首个终止）
- 每条错误带行号（`Type error at line N: ...`）
- 未知类型 / 跨模块 task 视为 Any，不强制检查
- `MORA_NO_TYPECK=1` 跳过 typeck，让动态行为生效

## LSP 语言服务

v11 配套 `mora-lsp` binary，提供完整 IDE 支持。**零外部依赖**——JSON-RPC 帧解析、JSON 序列化全部手写。

```bash
# 启动 LSP server（用编辑器配置为 LSP 启动命令）
mora-lsp
```

**支持能力**（与 `initialize` 响应中的 capabilities 一致）：
- `textDocumentSync`（full sync）
- `hover` — 悬停看变量/task 类型签名
- `completion` — 关键字 + 变量 + task + builtin
- `definition` — go-to-definition
- `references` — 查找所有引用
- `documentSymbol` — 大纲
- `documentFormatting` + `documentRangeFormatting` — 基础缩进格式化
- `rename` — 跨引用重命名
- `semanticTokens` — 语法高亮增强
- `foldingRange` — if/for/task 块折叠
- `publishDiagnostics` — typeck 错误推送

**端到端测试**：`cargo run --example lsp_smoke` 会启动 `mora-lsp` 子进程 + 模拟 LSP 客户端，验证 typeck diagnostics / hover / completion。

## 编辑器集成

详见 [editors/](./editors/README.md)。提供 6 个主流编辑器的即用配置：

| 编辑器 | 难度 | 关键文件 |
|--------|------|----------|
| [VS Code](./editors/vscode/) | 中（VSIX） | `package.json` + TextMate grammar |
| [Neovim](./editors/neovim/) | 低 | `lua/mora-lsp.lua` |
| [Helix](./editors/helix/) | 低 | `languages.toml` |
| [Sublime Text](./editors/sublime/) | 低 | `mora.sublime-settings` |
| [Vim](./editors/vim/) | 低 | `ftplugin/mora.vim` |
| [Emacs](./editors/emacs/) | 低 | `mora-mode.el` |

CI 自动构建多平台 mora + mora-lsp 二进制并发布到 GitHub Releases（见 `.github/workflows/release.yml`）。

## 架构

```
源码 .mora → 词法分析器 → Token 流 → 解析器 → AST ─┬─→ 解释器 → Value
                                                    │    ├── ai.stream/chat/tool/route/budget
                                                    │    ├── memory.store/recall
                                                    │    ├── agent.create().run()
                                                    │    └── web.fetch / json.* / file.*
                                                    └─→ 类型检查器 → 错误报告
                                                    └─→ LSP 语言服务 → 编辑器
```

- **词法分析器**（`src/lexer.rs`）—— 手写字符扫描器，token 携带行号+列号
- **解析器**（`src/parser.rs`）—— 递归下降，运算符优先级爬升
- **AST**（`src/ast.rs`）—— 14 种语句类型，10 种表达式类型，所有节点带 Span
- **解释器**（`src/interpreter.rs`）—— 树遍历执行，AI 原生内置模块
- **类型检查器**（`src/typeck.rs`）—— 编译期静态检查，多错误一次报告
- **LSP 服务**（`src/lsp/`）—— 零外部依赖 JSON-RPC，hover/completion/rename/diagnostics

## 许可证

BSD-3-Clause

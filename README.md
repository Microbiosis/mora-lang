# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)
[![CI](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml)

一个轻量级脚本语言，内建 AI 调用（`p"..."` 表达式）、HTTP server、MCP server、长期记忆、Agent 编排。

**v0.23**: 融合 9 个语言的设计基因 + 强类型升级 (Prolog、StreamIt、APL、Clojure、Lisp、Smalltalk、Common Lisp、Ballerina、Logo)

```mora
-- 一段代码 = HTTP + MCP + 可观测 (v0.04)
observe trace

route fast: ai_model("gpt-4o-mini", temperature: 0.7)
route deep: ai_model("gpt-4o")

serve as http on port 3000 do
  GET "/health" -> fn(req)
    return {status: "ok", version: "v0.04"}
  end

  POST "/chat" -> fn(req)
    span "user_chat" tags {path: "/chat"} do
      let text = req["body"]["text"]
      let answer = deep(p"用 deep 模型回答: {text}")
      record_tokens(120, answer.len())
      return answer
    end
  end
end

serve as mcp do
  tool search(query: string): string do
    return "found docs for: " + args["query"]
  end
end

-- 跑 mora script.mora
```

## 安装

从 [Releases](https://github.com/Microbiosis/mora-lang/releases/latest) 下载对应平台的二进制包（包含 `mora` + `mora-lsp`），解压后即可使用。

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
| 闭包 | `fn(x) return x + 1 end` |
| 列表 | `[1, 2, 3]`，`list.map(fn)`，`list.filter(fn)`，`list.reduce(fn, init)` |
| 字典 | `{key: val}`，`dict.get("key")` |
| 字符串 | `.len()`、`.upper()`、`.lower()`、`.trim()`、`.split()`、`.contains()`、`.replace()` |
| 流程控制 | `if/then/end`、`for x in list/end`、`match expr with/end` |
| 管道运算符 | `data \|> func()` |
| 并行执行 | `parallel ... end` |
| 模块系统 | `import "path"`，`export let/task` |

### v0.16-v0.20 新特性 (9 语言融合)

| 特性 | 来源 | 语法 |
|------|------|------|
| **守卫条件** | Prolog | `match n with x when x > 0 -> ... end` |
| **列表 rest** | Prolog | `let [head, ...tail] = [1, 2, 3]` |
| **管道闭包** | StreamIt | `5 \|> fn(x) return x * 2 end` |
| **窗口聚合** | StreamIt | `[1,2,3,4,5].window(3)` → `[[1,2,3],[2,3,4],[3,4,5]]` |
| **数组操作** | APL | `.shape()`、`.flatten()`、`.transpose()`、`.reshape()` |
| **广播算术** | APL | `[1,2,3] * 2` → `[2,4,6]` |
| **组合函数** | Clojure | `compose(f, g, h)` |
| **部分应用** | Lisp | `partial(add, 10)` |
| **原子引用** | Clojure | `atom(0)`、`swap()`、`deref()` |
| **运行时反射** | Smalltalk | `type_of()`、`is_instance()`、`methods_of()` |
| **用户宏** | Common Lisp | `macro name(params) ... end` |
| **Worker 并发** | Ballerina | `parallel worker w1 ... end end` |
| **事务支持** | Ballerina | `transaction ... compensation ... end` |

### 标准库

| 模块 | 功能 |
|------|------|
| `json.*` | `json.parse(text)`，`json.stringify(value)` |
| `web.*` | `web.fetch(url)`（真实 HTTP，基于 ureq） |
| `file.*` | `read_text/write_text/append_text/read_bytes/write_bytes`、`exists/is_file/is_dir/size/list/mkdir/remove/rename/copy/touch`、`cwd/chdir/home_dir/join/abs/basename/dirname/extname` |
| 持久化 | `save "file.json", value`，`load "file.json", var` |
| 文件语法糖 | `read "a.txt" into x`，`write "a.txt", content`，`append "a.txt", content` |

### v1.0 方向与项目哲学

Mora **永远不会到达 v1.0**,但**永远在逼近 v1.0**。

- v1.0 的方向是真实的:形式化语义、Hindley-Milner 推断、向量嵌入、长期记忆、SemVer API 稳定性等
- 这些方向的工作**持续在做**,从 v0.13 起每个版本都推进一部分
- 因为 v1.0 不到达,所以这些工作**永远做不完,永远在优化**
- 这是项目基因的一部分 —— 不是"以后再说"的托词,是"现在就在做但永远在路上"的诚实

表中标 🔄 的项 = 正在逼近 v1.0 但仍在演进,不代表"推迟",代表"持续优化方向"。

### AI 调用

> ⚠️ **v0.04 不兼容 v0.03**。下列 v0.03 builtin **已删除**，
> 调用会得到运行时错误 `Unknown method: ai.xxx` / `Unknown method: memory.xxx`：
>
> - `ai.chat` / `ai.stream` / `ai.tool` / `ai.route` / `ai.budget` / `ai.usage`
> - `ai.embed` / `ai.cosine` / `ai.dot` / `ai.euclidean` / `ai.norm` / `ai.search`
> - `memory.store` / `memory.recall` / `memory.forget` / `memory.clear` / `memory.list` / `memory.len`
>
> 替代见下表。

v0.04 把 AI 调用做成语法原语（`p"..."` / `with` / `stream` / `tool` / `catch e: AiError`）。
`ai.create` 和 `agent.critic` 仍保留，按值接收，不按方法调用风格。

| 特性 | 语法 | v0.04 状态 |
|------|------|-----------|
| AI 对话 | `p"hello"` | ✅ 表达式 |
| 多轮对话 | `let conv = ai.create("model"); conv.chat("...")` | ✅ 保留 |
| 流式输出 | `for token in ai.stream(p"...")` | ✅ 保留（注：mock 模式按字符拆 token，v0.04.1 跟进真实 SSE） |
| 工具调用 | `tool name(args): T do ... end` | ✅ 顶层语句 |
| 上下文配置 | `with model = "..." / budget = N` | ✅ 块语句 |
| AI 错误 | `try ... catch e: AiError` | ✅ 类型化错误，注入 dict `{message, code, retryable, attempts, cause}` |
| 显式 token 计数 | `record_tokens(input, output)` | ✅ 顶层语句 |
| 向量嵌入 | `ai.embed(text \| list)` | 🔄 逼近 v1.0 持续方向 |
| 向量运算 | `ai.cosine/dot/euclidean/norm` | 🔄 逼近 v1.0 持续方向 |
| 语义检索 | `ai.search(query, corpus, k?)` | 🔄 逼近 v1.0 持续方向 |
| 多模型路由 | `route fast: ai_model("gpt-4o-mini")` | ✅ 块语句（旧写法 `route fast: "gpt-4o-mini"` 仍兼容） |
| Token 预算 | `with budget = N` + `observe trace` | ✅ 由 `with` 配置 + observe metrics 替代 |
| Token 用量查询 | `observe trace` / `observe metrics` 块内置 | ✅ 由 observe 块 metrics 替代 `ai.usage()` |
| 长期记忆 | `memory.store/recall/forget/...` | 🔄 逼近 v1.0 持续方向 |
| Agent 编排 | `agent.create(name, config).run(task)` | ✅ 保留 |
| 输出评估 | `agent.critic(text)` / `agent.critic(text, ctx)` | ✅ 保留 |

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用真实 AI 调用 | （空 = mock 模式） |
| `MORA_AI_MODEL` | AI 模型名称 | `gpt-4o-mini` |
| `MORA_AI_BASE_URL` | API 端点地址 | `https://api.openai.com/v1` |
| `MORA_EMBED_MODEL` | Embedding 模型 | `text-embedding-3-small` |
| `MORA_NO_TYPECK` | 设为 `1` 跳过静态类型检查 | （空 = 启用） |

## 云服务部署

v0.04 把 HTTP server、MCP server、可观测放进语言层。脚本可以同时起 HTTP + MCP + trace。

### `serve` 块

```mora
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

跑 `mora script.mora` 同时起 HTTP (3000) + MCP (stdio) + trace。

| 协议 | 关键字 | 用途 |
|------|--------|------|
| HTTP | `serve as http on port N` | REST API (动态路由) |

**v0.11 起**：HTTP server 启动时如果 `port N` 被占，会自动试 `N+1, N+2, N+3`（最多 4 个端口），
并设置 `SO_REUSEADDR`（允许重用 TIME_WAIT 状态的端口）。如果 4 个端口都被占，server 启动失败。
控制台会打印实际监听的端口，例如 `[serve] requested port 3000 unavailable, using 3001 instead`。
| MCP | `serve as mcp` | Claude Desktop 等 MCP 客户端 |
| REPL | `serve as repl` | 进入交互式 REPL |
| Stdio | `serve as stdio` | echo 占位（v0.04.1 跟进自定义协议） |

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

`mora-lsp` binary 提供 IDE 支持。JSON-RPC 帧解析、JSON 序列化手写。

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
- **LSP 服务**（`src/lsp/`）—— 手写 JSON-RPC，hover/completion/rename/diagnostics

## 许可证

BSD-3-Clause

# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)

一门轻量级 Lua 风格的 AI 工作流脚本语言。

```mora
-- 解析 JSON
let data = json.parse("{\"name\":\"Mora\",\"version\":10}")
print(data.get("name"))  -- Mora

-- 文件读写
write "output.txt", "Hello Mora"
let content = file.read_text("output.txt")

-- 向量嵌入 + 语义检索
let results = ai.search("机器学习", ["AI入门", "烹饪教程", "深度学习"], 2)

-- AI 对话（需设置 OPENAI_API_KEY）
let answer = ai.chat("你好！")

-- 闭包 + 管道
["hello", "world", "mora"]
  |> map(fn(s) s.upper() end)
  |> filter(fn(s) s.len() > 4 end)
```

## 安装

从 [Releases](https://github.com/Microbiosis/mora-lang/releases/tag/v0.02) 下载对应平台的二进制包（包含 `mora` + `mora-lsp`），解压后即可使用。

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
| JSON | `json.parse(text)`，`json.stringify(value)` |
| HTTP | `web.fetch(url)`（真实请求，基于 ureq） |
| AI 对话 | `ai.chat(prompt)`，`ai.create(model?)`（兼容 OpenAI API） |
| 向量嵌入 | `ai.embed(text \| list, dim?)`（真实 OpenAI `/v1/embeddings`，默认 `text-embedding-3-small`） |
| 向量运算 | `ai.cosine(a, b)`，`ai.dot(a, b)`，`ai.euclidean(a, b)`，`ai.norm(v)` |
| 语义检索 | `ai.search(query, corpus, k?)`（无 key 走 mock 词袋兜底） |
| 持久化 | `save "file.json", value`，`load "file.json", var` |
| 文件系统 | `file.read_text/write_text/append_text/read_bytes/write_bytes`、`file.exists/is_file/is_dir/size/list/mkdir/mkdir_all/remove/remove_all/rename/copy/touch`、`file.cwd/chdir/home_dir/join/abs/basename/dirname/extname` |
| 文件语法糖 | `read "a.txt" into x`，`write "a.txt", content`，`append "a.txt", content`，`read_bytes "a.bin" into h`，`write_bytes "a.bin", hex` |

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用真实 AI 调用 | （空 = mock 模式） |
| `MORA_AI_MODEL` | AI 模型名称 | `gpt-4o-mini` |
| `MORA_AI_BASE_URL` | API 端点地址 | `https://api.openai.com/v1` |
| `MORA_EMBED_MODEL` | Embedding 模型 | `text-embedding-3-small` |
| `MORA_NO_TYPECK` | 设为 `1` 跳过静态类型检查 | （空 = 启用） |

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
                                                    └─→ 类型检查器 → 错误报告
                                                    └─→ LSP 语言服务 → 编辑器
```

- **词法分析器**（`src/lexer.rs`）—— 手写字符扫描器，支持转义序列，token 携带行号+列号
- **解析器**（`src/parser.rs`）—— 递归下降，运算符优先级爬升
- **AST**（`src/ast.rs`）—— 14 种语句类型，10 种表达式类型，所有节点带 Span
- **解释器**（`src/interpreter.rs`）—— 树遍历执行，`Arc<Mutex<Environment>>` 线程安全
- **类型检查器**（`src/typeck.rs`）—— 编译期静态检查，多错误一次报告
- **LSP 服务**（`src/lsp/`）—— 零外部依赖 JSON-RPC，支持 hover/completion/rename/diagnostics 等

## 许可证

BSD-3-Clause

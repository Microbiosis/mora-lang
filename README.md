# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628918.svg)](https://doi.org/10.5281/zenodo.20628918)

一门轻量级 Lua 风格的 AI 工作流脚本语言。

```mora
-- 解析 JSON
let data = json.parse("{\"name\":\"Mora\",\"version\":10}")
print(data.get("name"))  -- Mora

-- 真实 HTTP + JSON 链式调用
let resp = web.fetch("https://httpbin.org/uuid")
let uuid = json.parse(resp).get("uuid")

-- AI 对话（需设置 OPENAI_API_KEY）
let answer = ai.chat("你好！")

-- 多轮对话（带记忆）
let conv = ai.create("gpt-4o-mini")
conv.chat("我叫小明")
conv.chat("我叫什么名字？")  -- AI 记得叫小明

-- 闭包 + 管道
["hello", "world", "mora"]
  |> map(fn(s) s.upper() end)
  |> filter(fn(s) s.len() > 4 end)
```

## 快速开始

```bash
# 编译
cargo build --release

# 运行脚本
cargo run -- examples/ai_workflow.mora

# 交互式 REPL
cargo run -- --repl
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
| 持久化 | `save "file.json", value`，`load "file.json", var` |

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用真实 AI 调用 | （空 = mock 模式） |
| `MORA_AI_MODEL` | AI 模型名称 | `gpt-4o-mini` |
| `MORA_AI_BASE_URL` | API 端点地址 | `https://api.openai.com/v1` |

## 架构

```
源码 .mora → 词法分析器 → Token 流 → 解析器 → AST → 解释器 → Value
```

- **词法分析器**（`src/lexer.rs`）—— 手写字符扫描器，支持转义序列
- **解析器**（`src/parser.rs`）—— 递归下降，运算符优先级爬升
- **AST**（`src/ast.rs`）—— 14 种语句类型，10 种表达式类型
- **解释器**（`src/interpreter.rs`）—— 树遍历执行，`Arc<Mutex<Environment>>` 线程安全

唯一外部依赖：[`ureq`](https://docs.rs/ureq)（同步 HTTP 客户端）。

## 许可证

BSD-3-Clause

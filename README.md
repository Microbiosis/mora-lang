# Mora

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20628917.svg)](https://doi.org/10.5281/zenodo.20628917)
[![Release](https://img.shields.io/github/v/release/Microbiosis/mora-lang)](https://github.com/Microbiosis/mora-lang/releases/latest)
[![CI](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/Microbiosis/mora-lang/actions/workflows/ci.yml)

**Mora**  **AI-native **—— LLM HTTP/MCP  Agent 

**v0.51** 
- `p"..."`  AI `ai.chat` / `ai.embed` / `ai.critic` / `ai.retry` 
- v0.52+  MIR   10×
- 0  JSON/serde  JSON Value + 
- 4 build / **606 ** / fmt / clippy `-D warnings`

**v0.50** Pregel BSP 21 `state` / `node` / `channel` / `checkpoint` / `command` / `send` / `interrupt` / `rewind` / ... LangGraph  multi-agent + checkpoint 

**v0.45** ToolPlane Core/Extension adapterMIR α.0 α.2 Import/Parallel/With SHA-256 hash-chained audit log

[](docs/ARCHITECTURE.md) · [CHANGELOG](CHANGELOG.md) · [](docs/influences.md)

```mora
--  = HTTP + MCP +  (v0.04)
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
      let answer = deep(p" deep : {text}")
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

--  mora script.mora
```

## 

 [Releases](https://github.com/Microbiosis/mora-lang/releases/latest)  `mora` + `mora-lsp`

|  |  |
|------|------|
| Windows x86_64 | `mora-x86_64-pc-windows-msvc.zip` |
| Linux x86_64 (glibc) | `mora-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (musl) | `mora-x86_64-unknown-linux-musl.tar.gz` |
| macOS Intel | `mora-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `mora-aarch64-apple-darwin.tar.gz` |

 `mora`  PATH `mora` 

## 

```bash
# 
mora script.mora

#  REPL
mora --repl

# 
mora --check script.mora

#  LSP 
mora-lsp

#  Rust 
cargo build --release
```

## 

### 

|  |  |
|------|------|
|  | `let x = 1``let s: string = ""` |
|  | `task foo(x: string): string ... end` |
|  | `fn(x) return x + 1 end` |
|  | `[1, 2, 3]``list.map(fn)``list.filter(fn)``list.reduce(fn, init)` |
|  | `{key: val}``dict.get("key")` |
|  | `.len()``.upper()``.lower()``.trim()``.split()``.contains()``.replace()` |
|  | `if/then/end``for x in list/end``match expr with/end` |
|  | `data \|> func()` |
|  | `parallel ... end` |
|  | `import "path"``export let/task` |

### v0.16-v0.24 

|  |  |  |  |
|------|------|------|------|
| **v0.16** |  | Prolog | `match n with x when x > 0 -> ... end` |
| **v0.16** |  rest | Prolog | `let [head, ...tail] = [1, 2, 3]` |
| **v0.17** |  | StreamIt | `5 \|> fn(x) return x * 2 end` |
| **v0.17** |  | StreamIt | `[1,2,3,4,5].window(3)` |
| **v0.17** |  | APL | `.shape()``.flatten()``.transpose()``.reshape()` |
| **v0.17** |  | APL | `[1,2,3] * 2` → `[2,4,6]` |
| **v0.18** |  | Clojure | `compose(f, g, h)` |
| **v0.18** |  | Lisp | `partial(add, 10)` |
| **v0.19** |  | Clojure | `atom(0)``swap()``deref()` |
| **v0.19** | Worker  | Ballerina | `parallel worker w1 ... end end` |
| **v0.19** |  | Ballerina | `transaction ... compensation ... end` |
| **v0.20** |  | Smalltalk | `type_of()``is_instance()``methods_of()` |
| **v0.20** |  | Common Lisp | `macro name(params) ... end` |
| **v0.21** |  | Rust | `&expr``&mut expr` |
| **v0.21** |  | Rust | `<'a>`  |
| **v0.22** | AI  |  |  prompt  |
| **v0.22** |  |  |  |
| **v0.22** |  |  |  |
| **v0.24** |  |  | `type Name = TargetType` |
| **v0.24** |  |  | `enum Name { V1, V2(Type) }` |
| **v0.24** |  |  | `struct Name { field: Type }` |

### 

|  |  |
|------|------|
| `json.*` | `json.parse(text)``json.stringify(value)` |
| `web.*` | `web.fetch(url)` HTTP ureq |
| `file.*` | `read_text/write_text/append_text/read_bytes/write_bytes``exists/is_file/is_dir/size/list/mkdir/remove/rename/copy/touch``cwd/chdir/home_dir/join/abs/basename/dirname/extname` |
|  | `save "file.json", value``load "file.json", var` |
|  | `read "a.txt" into x``write "a.txt", content``append "a.txt", content` |

### v1.0 

Mora ** v1.0**,** v1.0**

- v1.0 :Hindley-Milner SemVer API 
- ****, v0.13 
-  v1.0 ,**,**
-  —— "",""

   =  v1.0 ,"",""

### AI 

>  **v0.04  v0.03** v0.03 builtin ****
>  `Unknown method: ai.xxx` / `Unknown method: memory.xxx`
>
> - `ai.chat` / `ai.stream` / `ai.tool` / `ai.route` / `ai.budget` / `ai.usage`
> - `ai.embed` / `ai.cosine` / `ai.dot` / `ai.euclidean` / `ai.norm` / `ai.search`
> - `memory.store` / `memory.recall` / `memory.forget` / `memory.clear` / `memory.list` / `memory.len`
>
> 

v0.04  AI `p"..."` / `with` / `stream` / `tool` / `catch e: AiError`
`ai.create`  `agent.critic` 

|  |  | v0.04  |
|------|------|-----------|
| AI  | `p"hello"` |   |
|  | `let conv = ai.create("model"); conv.chat("...")` |   |
|  | `for token in ai.stream(p"...")` |  mock  tokenv0.04.1  SSE |
|  | `tool name(args): T do ... end` |   |
|  | `with model = "..." / budget = N` |   |
| AI  | `try ... catch e: AiError` |   dict `{message, code, retryable, attempts, cause}` |
|  token  | `record_tokens(input, output)` |   |
|  | `ai.embed(text \| list)` |   v1.0  |
|  | `ai.cosine/dot/euclidean/norm` |   v1.0  |
|  | `ai.search(query, corpus, k?)` |   v1.0  |
|  | `route fast: ai_model("gpt-4o-mini")` |   `route fast: "gpt-4o-mini"`  |
| Token  | `with budget = N` + `observe trace` |   `with`  + observe metrics  |
| Token  | `observe trace` / `observe metrics`  |   observe  metrics  `ai.usage()` |
|  | `memory.store/recall/forget/...` |   v1.0  |
| Agent  | `agent.create(name, config).run(task)` |   |
|  | `agent.critic(text)` / `agent.critic(text, ctx)` |   |

## 

|  |  |  |
|------|------|--------|
| `OPENAI_API_KEY` |  AI  |  = mock  |
| `MORA_AI_MODEL` | AI  | `gpt-4o-mini` |
| `MORA_AI_BASE_URL` | API  | `https://api.openai.com/v1` |
| `MORA_EMBED_MODEL` | Embedding  | `text-embedding-3-small` |
| `MORA_NO_TYPECK` |  `1`  |  =  |

## 

v0.04  HTTP serverMCP server HTTP + MCP + trace

### `serve` 

```mora
observe trace

route fast: "gpt-4o-mini"
route deep: "gpt-4o"

serve as http on port 3000 do
  GET "/health" -> fn(req) return {status: "ok"} end
  POST "/chat"  -> fn(req) return deep(p" deep : {req.body.text}") end
end

serve as mcp do
  tool search(query: string): string do
    return "found: " + args["query"]
  end
end
```

 `mora script.mora`  HTTP (3000) + MCP (stdio) + trace

|  |  |  |
|------|--------|------|
| HTTP | `serve as http on port N` | REST API () |

**v0.11 **HTTP server  `port N`  `N+1, N+2, N+3` 4 
 `SO_REUSEADDR` TIME_WAIT  4 server 
 `[serve] requested port 3000 unavailable, using 3001 instead`
| MCP | `serve as mcp` | Claude Desktop  MCP  |
| REPL | `serve as repl` |  REPL |
| Stdio | `serve as stdio` | echo v0.04.1  |

### `route`  ()

```mora
route fast: "gpt-4o-mini"
route deep: "gpt-4o"

let s = fast(p"summarize: {text}")
let a = deep(p"analyze: {question}")
```

`fast(p"...")`  `gpt-4o-mini`, `deep(p"...")`  `gpt-4o`

### `observe` / `span`  ()

```mora
observe trace
observe otel endpoint "http://otel-collector:4317"

span "user_request" tags {user_id: u.id} do
  let r = deep(p"...")
  record_tokens(input, output)
end
```

`observe`  trace, `span` RAII `record_tokens`  token

## 

Mora v11 

```mora
let name: string = "mora"          --  
let age: number = "thirty"         --  typeck: string → number
task add(a: number, b: number): number
  return a + b
end
add(1, 2)                          -- 
add(1)                             --   2 
add("x", 2)                        --   1  number
```

****
- 
- `Type error at line N: ...`
-  /  task  Any
- `MORA_NO_TYPECK=1`  typeck

## LSP 

`mora-lsp` binary  IDE JSON-RPC JSON 

```bash
#  LSP server LSP 
mora-lsp
```

**** `initialize`  capabilities 
- `textDocumentSync`full sync
- `hover` — /task 
- `completion` —  +  + task + builtin
- `definition` — go-to-definition
- `references` — 
- `documentSymbol` — 
- `documentFormatting` + `documentRangeFormatting` — 
- `rename` — 
- `semanticTokens` — 
- `foldingRange` — if/for/task 
- `publishDiagnostics` — typeck 

****`cargo run --example lsp_smoke`  `mora-lsp`  +  LSP  typeck diagnostics / hover / completion

## 

 [editors/](./editors/README.md) 6 

|  |  |  |
|--------|------|----------|
| [VS Code](./editors/vscode/) | VSIX | `package.json` + TextMate grammar |
| [Neovim](./editors/neovim/) |  | `lua/mora-lsp.lua` |
| [Helix](./editors/helix/) |  | `languages.toml` |
| [Sublime Text](./editors/sublime/) |  | `mora.sublime-settings` |
| [Vim](./editors/vim/) |  | `ftplugin/mora.vim` |
| [Emacs](./editors/emacs/) |  | `mora-mode.el` |

CI  mora + mora-lsp  GitHub Releases `.github/workflows/release.yml`

## 

```
 .mora →  → Token  → ParserV2 → ASTv2 →  → AST →  → Value
                                                             ai.stream/chat/tool/route/budget
                                                             memory.store/recall
                                                             agent.create().run()
                                                             web.fetch / json.* / file.*
                                                        →  → 
                                                        → LSP  → 
```

- ****`src/lexer.rs`—— token +
- ****`src/parser_v2.rs`—— Arena  ast_v2 
- **AST v2**`src/ast_v2.rs`—— NodeId Pattern/ObserveConfig/FnDef 
- ****`src/ast_v2_to_v1.rs`——  ast_v2  ast
- ****`src/interpreter.rs`—— AI 
- ****`src/typeck.rs`—— 
- **LSP **`src/lsp/`——  JSON-RPChover/completion/rename/diagnostics

## 

BSD-3-Clause

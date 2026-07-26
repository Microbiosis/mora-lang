# Haxe / Feng / Heaps  —  mora 

> ****2026-07-08
> ****[Haxe](https://haxe.org)+ [Haxe std](https://github.com/HaxeFoundation/haxe/tree/master/std)+ [feng](https://github.com/cossbow/feng) OO + [Heaps](https://github.com/HeapsIO/heaps)
> **** mora 
> ****WebFetch  4  + WebSearch  mora v0.51 
> **** `RESEARCH_PRIMITIVES_MASTER_v2.md`17  Haxe 

---

## 0. 

|  |  |  mora  |
|------|------|----------------------|
| **Haxe** |  | Enum ADT  /  / Abstract newtype /  /  / DCE /  std  |
| **feng** |  OO  C++ | ARC + Resource class / phantom reference / read-only  / non-null  / C FFI bridgec2feng |
| **Heaps** | Haxe  | h2d/h3d/hxd/hxsl/ shader  DSL |
| **Haxe std** |  | Array/Map+ haxe/Http/Json/Timer+ sys/File/Process sys target+ target js.Browser/cpp.vm |

---

## 1. 

### 1.1 Haxe 

**Enum ADT**——Haxe  `enum`  ADT
```haxe
enum Color { RGB(r:Int, g:Int, b:Int); HSL(h:Int, s:Float, l:Float); Grayscale(v:Int); }
```
 `switch` ****——[Haxe Manual](https://haxe.org/manual/)

**structural typing**——
```haxe
typedef Point = { x: Float, y: Float };
//  x:Float + y:Float  Point
```
 + [Haxe Manual](https://haxe.org/manual/)

**Abstract types newtype**——
```haxe
abstract UserId(Int) from Int to Int { ... }
abstract OrderId(Int) from Int to Int { ... }
// UserId  OrderId  Int
```
 `@:to`/`@:from`  + `@:op` [Haxe Manual](https://haxe.org/manual/)

### 1.2 Haxe 

**Expression macro**——`macro`  AST 
```haxe
macro static function rand():Expr { return macro Std.random(100); }
```
 Haxe [Haxe Manual](https://haxe.org/manual/)

**Build macro@:build**—— `Field[]`/ORM/[Haxe Manual](https://haxe.org/manual/)

### 1.3 Haxe 

**IR →  target** →  → HIRHaxe IR→  target JS/C++/Java/C#/Python/Lua/HL/Neko/Eval target [Haxe GitHub](https://github.com/HaxeFoundation/haxe)

****`#if js`/`#elseif cpp`/`#end` 

**DCE** main + `@:keep` std/full/no JS  tree-shaking[Haxe Manual](https://haxe.org/manual/)

### 1.4 Haxe std 

[Haxe std ](https://daobook.github.io/haxe-book/docs/start/02_stdlib-intro.html)

|  |  |  |
|----|------|--------|
| **** | Array / Map / String / Math / EReg / Lambda / Reflect / Type / Xml |  target |
| **haxe/** | Http / Json / Timer / Serializer / Template / UnicodeString / crypto / ds / io |  target |
| **sys/** | Sys / sys.FileSystem / sys.io.File / sys.io.Process / sys.db / sys.thread |  sys targetC++/C#/Java/Neko/PHP |
| **target ** | js.Browser / cpp.vm / php.Session / python.Syntax / hl.* |  target |

### 1.5 feng 

[feng README](https://github.com/cossbow/feng)

- **ARC** retain/release GC
- **Resource class + **RAII 
- **Phantom reference** C++  / Rust 
- **read-only ** immutable
- **non-null ** null nullable
- **c2feng C FFI bridge**clang  .h  →  `extern "C"` wrapper + Feng/C 

### 1.6 Heaps 

[Heaps GitHub](https://github.com/HeapsIO/heaps)

- `h2d`2D Sprite/Drawable/Flow/Layout
- `h3d`3D Mesh/Camera/Material/Light
- `hxd`domain///
- `hxsl`shader  Haxe DSL shader 
- WebGL/OpenGL/DirectX/Flash/

---

## 2. mora 

| mora  | Haxe/feng  |  |
|----------------------|-------------------|------|
| v0.24  enum  match `statements.rs:261` "" | Enum ADT  |  mora enum  |
| Dict  `HashMap<String, Value>` |  `typedef P = {x:F}` |  Dict  |
| v0.24  type alias `TokenId`/`NodeId`/`AgentId`  usize | Abstract newtype | 🟡  |
| v0.20  | Expression macro  AST | 🟡  AST |
|  | `#if target` | 🟡  |
|  DCE | DCE  | 🟡  |
| Arc<Mutex>  | feng ARC + Resource class | 🟢  |
| let  reassignassign  | feng read-only  | 🟡  immutability |
| Value::Nil  non-null  | feng non-null  | 🟡  nullable  |
|  FFI | feng c2feng bridge | 🟢 Plateau D  |
|  | Haxe IR →  target | 🟢 Plateau C/D  |
| AI-nativep"..." + orchestrate | Haxe  |  **mora  AI ** |

---

## 3. 

###  P1 — Enum ADT  typeck

****Haxe Enum ADT + switch 
**mora **v0.24  `enum Name { V1, V2(Type) }` `match` `statements.rs:261` "" `expressions.rs:226`  `pattern()` ////——****
****
1. `match`  `expressions.rs:pattern()` 
2.  `enum`  match ****—— typeck 
3. enum `match e with RGB(r, g, b) -> ... end`
****mora  `AiError` / `FlowSignal` / `OrchestrateKind` / `StmtKind`  ADT
**** typeck + parser  C1/C2
****Plateau A"" bug

###  P2 —  /  Dict 

****Haxe `typedef Point = { x: Float, y: Float }`
**mora **Dict  `HashMap<String, Value>`HTTP handler / MCP tool  Dict
**** hinttypeck  Dict 
```mora
type Handler = { path: string, method: string }
task handle(req: Handler) -> string
  -- req  path + method 
end
```
****mora  HTTP/MCP/Agent  struct duck-type 
****typeck Dict 
****Plateau B HM 

###  P3 —  AI mora 

****Haxe expression macro AST× mora p"..." AI 
**mora **v0.20  `macro name(params) ... end` p"..."  AI
****" StmtKind "** AI**
```mora
macro generate_agent(role: string)
  --  AI  orchestrate 
  let code = ai.create("gpt-4o").chat(p" {role}  agent  orchestrate ")
  return parse_stmts(code)
end

-- 
generate_agent("reviewer")
```
****** mora ——Haxe mora  AI ** AI  token AI Agent  orchestrate  AI 
**** = typeck  AI C1syncAI  ai_chat  C2
**** AI ——record/replay CI  replay 
****Plateau B

### 🟡 P4 — Abstract newtype 

****Haxe `abstract UserId(Int) from Int to Int`
**mora **v0.24  `type Name = TargetType`alias alias ——`TokenId`  `NodeId`  usize 
**** type  abstract newtype
```mora
abstract TokenId(usize)
abstract NodeId(usize)
-- typeck  TokenId != NodeId usize
```
****mora  NodeId / TokenId / AgentId / StepId  AstArena  NodeId 
**** typeck
****Plateau A

### 🟡 P5 —  #if

****Haxe `#if js / #elseif cpp / #end`
**mora **
****
```mora
#if docker
  sandbox.containerize("agent-image")
#else
  --  fallback
  exec("echo", "no docker")
#end
```
****mora docker/mcp/lsp/repl
****lexer/parser 
****Plateau B

### 🟡 P6 — read-only / immutable 

****feng read-only 
**mora **let  reassignassign 
****`let const`  `let readonly`  reassignDict  readonly 
```mora
let const max_tokens = 4096
let readonly config = { model: "gpt-4o" }
-- config  reassign
```
****mora  worker readonly 
****typeck + execute 
****Plateau A

### 🟡 P7 — non-null / nullable 

****feng non-null 
**mora **Value::Nil  non-null 
****Type  `?T` nullable  non-null
```mora
task find(id: string) -> ?Agent  --  nil
task must_get(id: string) -> Agent  -- non-null nil
```
**** NPE typeck  nil 
****typeck 
****Plateau B HM 

### 🟢 P8 — DCE 

****Haxe DCE  + @:keep
**mora **
****mora  MCP server  VM  tree-shaking
**** VM 
****Plateau C VM 

### 🟢 P9 —  target

****Haxe IR →  target
**mora **
****C3" v1.0"
**mora "target"** VM / WASM /  IR Haxe  target
****Plateau C/D VM 

### 🟢 P10 — C FFI bridge

****feng c2fengclang  → bridge 
**mora ** FFI
****C2 
****Plateau D 

###  P11 — Heaps 

****mora  AI  DSL/ Heaps "shader  DSL"hxsl——mora  prompt p"..." DSL hxsl 

---

## 4. 

### 4.1 Enum ADT P1—— 

mora  agent 
- `statements.rs:261`  `match_statement` ""
- `expressions.rs:226`  `pattern()` ////

P1 ** match ** enum """" mora""


1. `match_statement`  `pattern()` 
2. typeck  enum  match  Haxe
3. enum 

### 4.2  AI P3—— mora 

****Haxe mora ** AI **—— AI-native 


```
mora script.mora ()
  → typeck  generate_agent("reviewer") 
  → ai.create("gpt-4o").chat(p" reviewer  orchestrate ")
  → AI  → parse_stmts → Vec<StmtKind>
  →  typeck AI 
  →  .mora/macro-cache/record/replay 
  →  replay CI 
```

****
- AI  → record/replay mora  record 
- CI  `MORA_AI_MOCK=1`  mock replay
- AI  typeck 

 mora "AI-native"p"..." + orchestrate——**AI  agent agent **

### 4.3 P2—— Dict 

mora  Dict  HashMap""
```mora
type HttpRequest = { method: string, path: string, body?: string }
task handler(req: HttpRequest) -> HttpResponse
  -- typeck  Dict  method + pathbody 
end
```

 struct Dict —— +  mora  HTTP/MCP handler 

---

## 5.  4 Plateau

|  | Plateau |  |
|------|---------|------|
| P1 Enum ADT  | A | match  typeck |
| P4 Abstract newtype | A |  NodeId/TokenId  typeck |
| P6 read-only  | A | execute + typeck |
| P2  | B | HM  |
| P3  AI  | B | mora  |
| P5  | B | lexer/parser  |
| P7 non-null  | B |  HM  |
| P8 DCE | C |  VM  |
| P9  target | C/D |  VM  |
| P10 C FFI | D |  |

---

## 6. Haxe  mora 

1. **Enum ADT P1**——mora  enum +  match  Plateau A ""

2. ** AI P3 mora **——Haxe mora  AI  AI-native  AI-native  record/replay 

3. **P2 Dict **——mora  Dict  HTTP/MCP handler "" +  struct

feng  ARC/Resource/non-null  mora mora  Arc ARC Heaps mora "shader  DSL""prompt  DSL "

****
- [Haxe Manual](https://haxe.org/manual/) — ///DCE
- [Haxe std ](https://daobook.github.io/haxe-book/docs/start/02_stdlib-intro.html) — 
- [feng README](https://github.com/cossbow/feng) — ARC/Resource/phantom ref/read-only/non-null/c2feng
- [Heaps GitHub](https://github.com/HeapsIO/heaps) — h2d/h3d/hxd/hxsl 

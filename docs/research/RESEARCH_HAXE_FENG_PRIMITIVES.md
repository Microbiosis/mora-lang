# Haxe / Feng / Heaps 原语研究 — 为 mora 提取互补性特性

> **研究日期**：2026-07-08
> **研究对象**：[Haxe](https://haxe.org)（跨平台编译语言）+ [Haxe std](https://github.com/HaxeFoundation/haxe/tree/master/std)（标准库）+ [feng](https://github.com/cossbow/feng)（内存安全 OO 语言）+ [Heaps](https://github.com/HeapsIO/heaps)（游戏框架）
> **目的**：解析实现原理，提取机制，为 mora 补充功能性、互补性特性和原语
> **方法**：WebFetch 抓取 4 个项目 + WebSearch 补充，对照 mora v0.51 现状（前几轮已深入分析）
> **配套**：与 `RESEARCH_PRIMITIVES_MASTER_v2.md`（17 项目研究）同类型，本文专注 Haxe 生态

---

## 0. 速览：四个项目的核心机制

| 项目 | 定位 | 对 mora 最有价值的机制 |
|------|------|----------------------|
| **Haxe** | 跨平台编译语言 | Enum ADT 穷尽匹配 / 结构子类型 / Abstract newtype / 编译期宏 / 条件编译 / DCE / 三层 std 抽象 |
| **feng** | 内存安全 OO 语言（编译到 C++） | ARC + Resource class / phantom reference / read-only 标记 / non-null 引用 / C FFI bridge（c2feng） |
| **Heaps** | Haxe 游戏框架 | 跨平台图形抽象分层（h2d/h3d/hxd/hxsl）/ shader 作为 DSL |
| **Haxe std** | 跨平台标准库 | 通用层（Array/Map）+ haxe/（Http/Json/Timer）+ sys/（File/Process，仅 sys target）+ target 特定（js.Browser/cpp.vm）三层分离 |

---

## 1. 机制深度提取

### 1.1 Haxe 类型系统

**Enum ADT（代数数据类型）**——Haxe 的 `enum` 不是常量枚举，是真正的 ADT：
```haxe
enum Color { RGB(r:Int, g:Int, b:Int); HSL(h:Int, s:Float, l:Float); Grayscale(v:Int); }
```
配合 `switch` 模式匹配，编译器**检查穷尽性**——漏一个分支编译失败。来源：[Haxe Manual](https://haxe.org/manual/)

**结构子类型（structural typing）**——类型兼容基于字段集而非名义继承：
```haxe
typedef Point = { x: Float, y: Float };
// 任何含 x:Float + y:Float 的对象自动兼容 Point
```
鸭子类型 + 编译期类型安全，无需显式继承。来源：[Haxe Manual](https://haxe.org/manual/)

**Abstract types（零成本 newtype）**——编译期包装，运行时透明：
```haxe
abstract UserId(Int) from Int to Int { ... }
abstract OrderId(Int) from Int to Int { ... }
// UserId 和 OrderId 编译期不可混用，运行时都是 Int
```
支持 `@:to`/`@:from` 隐式转换 + `@:op` 运算符重载。来源：[Haxe Manual](https://haxe.org/manual/)

### 1.2 Haxe 宏系统

**Expression macro**——`macro` 函数在编译期执行，返回 AST 插入调用处：
```haxe
macro static function rand():Expr { return macro Std.random(100); }
```
宏用 Haxe 自身编写，宏上下文与运行时隔离（不能访问运行时值），保证确定性。来源：[Haxe Manual](https://haxe.org/manual/)

**Build macro（@:build）**——类型构建阶段改写字段：接收 `Field[]`，可增删改后返回。用于自动生成序列化/ORM/依赖注入代码。来源：[Haxe Manual](https://haxe.org/manual/)

### 1.3 Haxe 跨平台编译

**IR → 多 target**：源码 → 类型检查 → HIR（Haxe IR）→ 各 target 生成器（JS/C++/Java/C#/Python/Lua/HL/Neko/Eval）。所有 target 共享同一套类型语义。来源：[Haxe GitHub](https://github.com/HaxeFoundation/haxe)

**条件编译**：`#if js`/`#elseif cpp`/`#end` 编译期选择分支，零运行时开销。

**DCE（死代码消除）**：以 main + `@:keep` 为根做可达性分析，三种模式（std/full/no），特别对 JS 输出做 tree-shaking。来源：[Haxe Manual](https://haxe.org/manual/)

### 1.4 Haxe std 三层抽象

来源：[Haxe std 简介](https://daobook.github.io/haxe-book/docs/start/02_stdlib-intro.html)

| 层 | 内容 | 可用性 |
|----|------|--------|
| **通用** | Array / Map / String / Math / EReg / Lambda / Reflect / Type / Xml | 所有 target |
| **haxe/** | Http / Json / Timer / Serializer / Template / UnicodeString / crypto / ds / io | 所有 target |
| **sys/** | Sys / sys.FileSystem / sys.io.File / sys.io.Process / sys.db / sys.thread | 仅 sys target（C++/C#/Java/Neko/PHP） |
| **target 特定** | js.Browser / cpp.vm / php.Session / python.Syntax / hl.* | 仅对应 target |

### 1.5 feng 内存安全机制

来源：[feng README](https://github.com/cossbow/feng)

- **ARC（自动引用计数）**：编译期插入 retain/release，无 GC
- **Resource class + 析构器**：RAII 资源管理
- **Phantom reference**：类似 C++ 引用 / Rust 借用，零开销
- **read-only 标记**：引用和方法可标 immutable，写保护无需封装
- **non-null 引用**：类型系统消除 null，显式 nullable
- **c2feng C FFI bridge**：clang 分析 .h 头 → 自动生成 `extern "C"` wrapper + 模块前缀，Feng/C 混合构建

### 1.6 Heaps 分层

来源：[Heaps GitHub](https://github.com/HeapsIO/heaps)

- `h2d`：2D 场景图（Sprite/Drawable/Flow/Layout）
- `h3d`：3D 渲染（Mesh/Camera/Material/Light）
- `hxd`：domain（资源/输入/音频/系统抽象）
- `hxsl`：shader 作为 Haxe DSL（编译期类型安全的 shader 编写）
- 多后端：WebGL/OpenGL/DirectX/Flash/主机

---

## 2. mora 现状对照

| mora 现状（前几轮核实） | Haxe/feng 对应机制 | 差距 |
|----------------------|-------------------|------|
| v0.24 有 enum 但 match 只支持变量模式（`statements.rs:261` 注释"简化"） | Enum ADT 穷尽匹配 | 🔴 mora enum 有壳无魂 |
| Dict 是 `HashMap<String, Value>`，无类型约束 | 结构子类型 `typedef P = {x:F}` | 🔴 Dict 无结构类型 |
| v0.24 有 type alias，但 `TokenId`/`NodeId`/`AgentId` 都是裸 usize | Abstract newtype | 🟡 无零成本区分 |
| v0.20 有用户宏但能力弱（文本替换级） | Expression macro 返回 AST | 🟡 宏不返回 AST |
| 无条件编译 | `#if target` | 🟡 缺失 |
| 无 DCE（所有定义加载） | DCE 可达性分析 | 🟡 解释器下价值有限 |
| Arc<Mutex> 隐式引用计数 | feng ARC + Resource class | 🟢 已隐式有 |
| let 可 reassign（assign 语句） | feng read-only 标记 | 🟡 无 immutability |
| Value::Nil 可选，无 non-null 类型 | feng non-null 引用 | 🟡 无 nullable 标注 |
| 无 FFI | feng c2feng bridge | 🟢 Plateau D 方向 |
| 无跨平台编译（只有解释器） | Haxe IR → 多 target | 🟢 Plateau C/D 远期 |
| AI-native（p"..." + orchestrate） | Haxe 宏编译期生成 | 🔴 **mora 独有机会：编译期 AI 宏** |

---

## 3. 互补性原语提议（按可落地性分级）

### 🔴 P1 — Enum ADT 穷尽性模式匹配（高价值，纯 typeck）

**来源**：Haxe Enum ADT + switch 穷尽性检查
**mora 现状**：v0.24 有 `enum Name { V1, V2(Type) }`，但 `match` 语句只支持变量模式（`statements.rs:261` 自述"简化：只支持变量模式"），而 `expressions.rs:226` 的 `pattern()` 已支持通配/字面量/列表/字典/守卫全模式——**同关键字两套能力，是已知不一致**。
**提议**：
1. `match` 语句复用 `expressions.rs:pattern()` 的全套模式能力
2. 对 `enum` 类型的 match 增加**穷尽性检查**——漏分支 typeck 报错
3. enum 构造子带参数时支持解构：`match e with RGB(r, g, b) -> ... end`
**价值**：mora 的 `AiError` / `FlowSignal` / `OrchestrateKind` / `StmtKind` 都该是 ADT，穷尽匹配能防漏分支
**约束兼容**：纯 typeck + parser 增强，不碰 C1/C2
**落地**：Plateau A（结构债清偿期间顺手做，因为它修的是"两套模式能力不一致"的已知 bug）

### 🔴 P2 — 结构子类型 / 结构化 Dict 类型（高价值）

**来源**：Haxe `typedef Point = { x: Float, y: Float }`
**mora 现状**：Dict 是 `HashMap<String, Value>`，无类型约束；HTTP handler / MCP tool 参数都是裸 Dict
**提议**：增加结构类型 hint，typeck 检查 Dict 是否含必需字段：
```mora
type Handler = { path: string, method: string }
task handle(req: Handler) -> string
  -- req 被静态检查必须含 path + method 字段
end
```
**价值**：mora 的 HTTP/MCP/Agent 接口能用结构类型声明，无需定义 struct；跨模块 duck-type 兼容
**约束兼容**：typeck 增强，Dict 运行时不变
**落地**：Plateau B（形式化期间，结构子类型是 HM 推断的自然延伸）

### 🔴 P3 — 编译期 AI 宏（mora 独有，最高价值）

**来源**：Haxe expression macro（编译期执行返回 AST）× mora p"..." AI 原语
**mora 现状**：v0.20 有 `macro name(params) ... end` 但能力弱；p"..." 只做字符串拼接（不触发 AI，前几轮已核实）
**提议**：增强宏为"编译期执行，返回 StmtKind 列表插入调用处"，且**宏体内可调 AI**：
```mora
macro generate_agent(role: string)
  -- 编译期调 AI 生成 orchestrate 代码
  let code = ai.create("gpt-4o").chat(p"为角色 {role} 生成一个 agent 的 orchestrate 代码")
  return parse_stmts(code)
end

-- 调用处：编译期展开
generate_agent("reviewer")
```
**价值**：**这是 mora 独有的——Haxe 宏生成普通代码，mora 宏能调 AI 生成代码**。把 AI 从运行时调用提升到编译时生成，减少运行时延迟和 token 消耗。AI Agent 的 orchestrate 图可在编译期由 AI 生成并静态检查。
**约束兼容**：宏在编译期执行 = typeck 阶段调 AI，不碰 C1（sync）；AI 调用走现有 ai_chat 路径，不碰 C2
**风险**：编译期 AI 调用引入非确定性（同样输入不同输出）——缓解：宏结果可缓存（record/replay 机制已有），CI 用 replay 模式保证可重现
**落地**：Plateau B（形式化期间，因为宏语义需要形式化定义）

### 🟡 P4 — Abstract newtype 零成本区分

**来源**：Haxe `abstract UserId(Int) from Int to Int`
**mora 现状**：v0.24 有 `type Name = TargetType`（alias），但 alias 不区分——`TokenId` 和 `NodeId` 都是 usize 可混用
**提议**：增强 type 为 abstract newtype，编译期区分，运行时透明：
```mora
abstract TokenId(usize)
abstract NodeId(usize)
-- typeck 阶段 TokenId != NodeId，混用报错；运行时都是 usize
```
**价值**：mora 的 NodeId / TokenId / AgentId / StepId 防混用（前几轮发现 AstArena 双数组共用 NodeId 易误用）
**约束兼容**：纯 typeck，运行时零开销
**落地**：Plateau A（小改动，顺手）

### 🟡 P5 — 条件编译 #if

**来源**：Haxe `#if js / #elseif cpp / #end`
**mora 现状**：无条件编译
**提议**：
```mora
#if docker
  sandbox.containerize("agent-image")
#else
  -- 本地开发 fallback
  exec("echo", "no docker")
#end
```
**价值**：mora 脚本按运行环境（docker/mcp/lsp/repl）裁剪，减少运行时分支
**约束兼容**：lexer/parser 增强
**落地**：Plateau B

### 🟡 P6 — read-only / immutable 绑定

**来源**：feng read-only 标记
**mora 现状**：let 可 reassign（assign 语句）
**提议**：`let const` 或 `let readonly` 绑定不可 reassign；Dict 可标 readonly 共享无需锁
```mora
let const max_tokens = 4096
let readonly config = { model: "gpt-4o" }
-- config 不可 reassign，字段不可改
```
**价值**：并发安全（mora 有 worker 并发），readonly 共享数据无需锁
**约束兼容**：typeck + execute 增强
**落地**：Plateau A

### 🟡 P7 — non-null / nullable 类型标注

**来源**：feng non-null 引用
**mora 现状**：Value::Nil 可选，无 non-null 标注
**提议**：Type 加 `?T` nullable 标记，默认 non-null：
```mora
task find(id: string) -> ?Agent  -- 可能返回 nil
task must_get(id: string) -> Agent  -- non-null，不返回 nil
```
**价值**：减少 NPE 类运行时错误，typeck 静态检查 nil 解引用
**约束兼容**：typeck 增强
**落地**：Plateau B（与 HM 推断一起）

### 🟢 P8 — DCE 死代码消除（远期）

**来源**：Haxe DCE 可达性分析 + @:keep
**mora 现状**：无，所有定义加载
**价值**：mora 脚本作为 MCP server 部署时减小体积；未来字节码 VM 的 tree-shaking
**约束**：解释器下价值有限（不生成代码），字节码 VM 后才有意义
**落地**：Plateau C（字节码 VM 之后）

### 🟢 P9 — 跨平台编译 target（远期）

**来源**：Haxe IR → 多 target
**mora 现状**：只有树遍历解释器
**约束冲突**：C3"永远不到 v1.0"，跨平台编译是大工程
**mora 的"target"可能是**：字节码 VM / WASM / 嵌入式 IR（而非 Haxe 那样的多语言 target）
**落地**：Plateau C/D，与字节码 VM 一起考虑

### 🟢 P10 — C FFI bridge（远期）

**来源**：feng c2feng（clang 头分析 → bridge 生成）
**mora 现状**：无 FFI
**约束冲突**：C2 零少依赖
**落地**：Plateau D 生态方向

### ⚪ P11 — Heaps 图形抽象（不适用）

**判定**：mora 是 AI 编排 DSL，不是游戏/图形语言，跳过。但 Heaps 的"shader 作为 DSL"（hxsl）思路可借鉴——mora 的 prompt 模板（p"..."）本质上也是一种 DSL，可学 hxsl 做编译期类型检查。

---

## 4. 重点原语详述

### 4.1 Enum ADT 穷尽匹配（P1）—— 修已知不一致

mora 已有的不一致（前几轮 agent 报告发现）：
- `statements.rs:261` 的 `match_statement` 注释"简化：只支持变量模式"
- `expressions.rs:226` 的 `pattern()` 已支持通配/字面量/列表/字典/守卫

P1 不是新功能，是**把已有的表达式模式能力接到语句 match 上**，再加 enum 穷尽检查。这是"补一致性"而非"加复杂度"，符合 mora"渐进复杂度"哲学。

落地步骤：
1. `match_statement` 改为调用 `pattern()` 而非自己的简化版
2. typeck 对 enum 类型的 match 做穷尽性检查（参考 Haxe：漏分支编译失败）
3. enum 构造子带参数时支持解构绑定

### 4.2 编译期 AI 宏（P3）—— mora 的差异化放大器

这是本研究**最独特的发现**。Haxe 宏在编译期生成代码，但生成的代码来自宏逻辑（确定性的）。mora 宏可以**在编译期调 AI 生成代码**——这是任何非 AI-native 语言做不到的。

工作流：
```
mora script.mora (编译期)
  → typeck 遇到 generate_agent("reviewer") 宏调用
  → 执行宏体：ai.create("gpt-4o").chat(p"为 reviewer 生成 orchestrate 代码")
  → AI 返回代码字符串 → parse_stmts → Vec<StmtKind>
  → 插入调用处，继续 typeck（静态检查 AI 生成的代码）
  → 结果缓存到 .mora/macro-cache/（record/replay 机制）
  → 下次编译用 replay 模式（CI 可重现）
```

**关键约束处理**：
- 非确定性：AI 每次返回可能不同 → record/replay 缓存保证可重现（mora 已有 record 机制）
- 编译期网络依赖：CI 环境 `MORA_AI_MOCK=1` 走 mock，或 replay
- 安全：AI 生成代码经 typeck 静态检查后才插入，不会绕过类型安全

这个原语让 mora 的"AI-native"从运行时（p"..." + orchestrate）延伸到编译时——**AI 不只执行 agent，还生成 agent 代码**。

### 4.3 结构子类型（P2）—— Dict 的类型安全

mora 的 Dict 当前是黑盒 HashMap。结构子类型让它可声明"形状"：
```mora
type HttpRequest = { method: string, path: string, body?: string }
task handler(req: HttpRequest) -> HttpResponse
  -- typeck 检查传入 Dict 是否含 method + path（body 可选）
end
```

这不需要定义 struct，任何含这些字段的 Dict 自动兼容——鸭子类型 + 静态安全。对 mora 的 HTTP/MCP handler 参数验证特别有用。

---

## 5. 落地路线（对接 4 Plateau）

| 原语 | Plateau | 理由 |
|------|---------|------|
| P1 Enum ADT 穷尽匹配 | A | 修已知不一致（match 语句模式能力缺失），纯 typeck |
| P4 Abstract newtype | A | 小改动，防 NodeId/TokenId 混用，纯 typeck |
| P6 read-only 绑定 | A | 并发安全，execute + typeck |
| P2 结构子类型 | B | HM 推断的自然延伸，形式化期间 |
| P3 编译期 AI 宏 | B | mora 差异化，需形式化宏语义 |
| P5 条件编译 | B | lexer/parser 增强 |
| P7 non-null 类型 | B | 与 HM 推断一起 |
| P8 DCE | C | 字节码 VM 后才有意义 |
| P9 跨平台 target | C/D | 与 VM 一起 |
| P10 C FFI | D | 生态方向 |

---

## 6. 结论：Haxe 生态给 mora 的三个关键启发

1. **Enum ADT 穷尽匹配（P1）不是新功能是补一致性**——mora 已有 enum + 已有表达式模式能力，只是 match 语句没接上。这是 Plateau A 顺手做的"一致性补全"，零新增复杂度。

2. **编译期 AI 宏（P3）是 mora 独有的差异化**——Haxe 宏生成代码，mora 宏调 AI 生成代码。这把 AI-native 从运行时延伸到编译时，是任何非 AI-native 语言做不到的。需配合 record/replay 缓存保证可重现。

3. **结构子类型（P2）让 Dict 类型安全**——mora 的 Dict 是核心容器但无类型约束。结构子类型让 HTTP/MCP handler 参数可声明"形状"，鸭子类型 + 静态安全，无需定义 struct。

feng 的 ARC/Resource/non-null 对 mora 价值较低（mora 已隐式用 Arc，脚本语言显式 ARC 过早）。Heaps 的图形抽象不适用（mora 不是游戏语言），但"shader 作为 DSL"的思路启发了"prompt 模板作为 DSL 可编译期检查"的方向。

**来源**：
- [Haxe Manual](https://haxe.org/manual/) — 类型系统/宏/编译机制/DCE
- [Haxe std 简介](https://daobook.github.io/haxe-book/docs/start/02_stdlib-intro.html) — 三层抽象
- [feng README](https://github.com/cossbow/feng) — ARC/Resource/phantom ref/read-only/non-null/c2feng
- [Heaps GitHub](https://github.com/HeapsIO/heaps) — h2d/h3d/hxd/hxsl 分层

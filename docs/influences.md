# Mora 语言设计影响源

> 9 个学习对象，每个贡献了一个核心设计基因。

---

## 1. Clojure — 不可变数据结构

**基因**: 值语义 + 持久化数据结构

```clojure
;; Clojure: 所有数据结构默认不可变
(def a [1 2 3])
(def b (conj a 4))   ;; a 不变，b 是新向量
```

**Mora 继承**:
```mora
let a = [1, 2, 3]
let b = a | push(4)    -- a 不变，b 是新列表
assign a = [10]        -- 显式修改，意图清晰
```

| Clojure 原则 | Mora 体现 |
|--------------|-----------|
| 默认不可变 | `let` 不可变，`assign` 显式修改 |
| 持久化数据结构 | `list`/`dict` 赋值深拷贝 |
| STM (软件事务内存) | v1.0 计划 |
| REPL 驱动开发 | `mora --repl` |

---

## 2. Common Lisp — 宏系统 + 多范式

**基因**: 代码即数据、卫生宏、条件系统

```lisp
;; Common Lisp: 宏在编译期变换 AST
(defmacro when (condition &body body)
  `(if ,condition (progn ,@body)))

;; 条件系统 (比 try/catch 更强)
(handler-case (error "oops")
  (error (c) (format t "caught: ~a" c)))
```

**Mora 继承**:
```mora
-- with 块：声明式上下文变换（类似宏的效果）
with model = "gpt-4o", temperature = 0.7
  let r = ai.chat("hello")
end

-- Result 类型：显式错误处理（类似 condition system 的理念）
let result = risky_call()
match result
  | Ok(v) => process(v)
  | Err(e) => handle_error(e)
end
```

| Common Lisp 原则 | Mora 体现 |
|------------------|-----------|
| 多范式共存 | 命令式 + 函数式 + OOP + 声明式 |
| REPL 优先 | `mora --repl` |
| 动态类型 → 静态类型 | Mora 选择静态类型（反向借鉴） |
| 宏作为 AST 变换 | `with`/`route`/`observe` 是语法级宏 |

---

## 3. Prolog — 模式匹配 + 逻辑编程

**基因**: 声明式推理、回溯、合一

```prolog
% Prolog: 模式匹配 + 回溯
factorial(0, 1).
factorial(N, F) :- N > 0, N1 is N - 1, factorial(N1, F1), F is N * F1.
```

**Mora 继承**:
```mora
-- 模式匹配（来自 Prolog 的合一思想）
let result = match value
  | 0 => "zero"
  | n: number => p"number: {n}"
  | [head, ...tail] => p"list with head: {head}"
  | {name, age} => p"person: {name}, {age}"
  | _ => "unknown"
end

-- with 块：声明式配置（逻辑编程的 "声明目标"）
with model = "gpt-4o"
  -- 声明 "在这个上下文中执行"
end
```

| Prolog 原则 | Mora 体现 |
|-------------|-----------|
| 模式匹配 | `match` 语句 |
| 声明式编程 | `with`/`route`/`observe` |
| 回溯 | `Result` + `?` 操作符（错误回溯） |
| 合一 | 结构化模式匹配 |

---

## 4. Lisp — 代码即数据 (Homoiconicity)

**基因**: S 表达式、宏、最小语法

```lisp
;; Lisp: 代码就是列表，列表就是代码
(+ 1 2)           ;; 调用
'(+ 1 2)          ;; 数据
(eval '(+ 1 2))   ;; 数据变代码
```

**Mora 继承（v0.86-v0.88 完整实现 homoiconicity）**:
```mora
-- eval: 运行时编译并执行 Mora 代码（v0.86）
let code = "let x = 10 + 20"
eval(code)           -- x = 30

-- quote: 表达式 → Value::Code 数据（v0.86）
let expr = quote(1 + 2)    -- Code，不求值

-- gensym: 生成唯一符号（v0.86）
let sym = gensym()          -- "g0", "g1", ...

-- read: 字符串 → Value::Code（v0.86）
let ast = read("let x = 42")

-- macroexpand: 展开宏调用（v0.86）
macroexpand("my_macro", [arg1, arg2])

-- quasiquote + unquote + unquote-splice（v0.88）
let items = [1, 2, 3]
let code = `(sum ,,items)     -- splicing

-- apply / car / cdr / cons（v0.86）
let list = cons(1, cons(2, nil))
let head = car(list)           -- 1
let tail = cdr(list)           -- (2 . nil)
```

| Lisp 原则 | Mora 体现 |
|-----------|-----------|
| 代码即数据 (homoiconicity) | `eval`/`quote`/`read` + `Value::Code` 类型 |
| 宏系统 | `macro_stmt` + `macroexpand` + quasiquote |
| 最小语法 | 30 个关键字，无花括号 |
| REPL | `mora --repl` |
| 函数是一等公民 | 闭包、高阶函数、`apply` |

---

## 5. Ballerina — 云原生集成

**基因**: 网络原语、服务声明、类型化网络交互

```ballerina
// Ballerina: 网络服务是一等公民
service /api on new http:Listener(8080) {
    resource function post chat(ChatRequest req) returns ChatResponse|error {
        // ...
    }
}
```

**Mora 继承**:
```mora
-- 路由声明：网络服务是一等公民
route POST /api/chat -> handle_chat
route GET /api/health -> handle_health

-- HTTP 服务器：零配置启动
let router = Router::new()
router
  | route("POST", "/api/chat", handle_chat)
  | listen("0.0.0.0:3000")

-- MCP 服务器：协议原生支持
let server = McpServer::new()
server
  | tool("greet", {name: {type: "string"}}, greet_handler)
  | serve()
```

| Ballerina 原则 | Mora 体现 |
|----------------|-----------|
| 网络原语内置 | `Router::new()`, `McpServer::new()` |
| 类型化网络交互 | `HttpRequest`, `HttpResponse` 类型 |
| 声明式服务 | `route` 语句 |
| 可观测性内置 | `observe`/`span` |

---

## 6. StreamIt — 流处理

**基因**: 数据流编程、流管道、生产者-消费者

```streamit
// StreamIt: 流是第一公民
pipeline MyFilter {
    add Source();
    add Filter();
    add Sink();
}
```

**Mora 继承**:
```mora
-- 管道运算符：流式数据处理
let result = "hello world"
  | upper()
  | split(" ")
  | map(fn(w) = w.trim())
  | filter(fn(w) = len(w) > 3)
  | reduce(fn(acc, w) = acc + " " + w, "")

-- AI 流式输出
let stream = ai.stream("Tell me a story")
for token in stream
  print(token)
end

-- Agent 多步执行流
let agent = ai.create("researcher", {tools: ["search"]})
let result = agent.run("Find latest AI news")
```

| StreamIt 原则 | Mora 体现 |
|---------------|-----------|
| 流是一等公民 | `stream` 类型、`stream for` |
| 管道组合 | `\|` 运算符 |
| 生产者-消费者 | `ai.stream` → `for` 循环 |
| 声明式数据流 | `map`/`filter`/`reduce` |

---

## 7. APL — 符号密度 + 数组编程

**基因**: 极致简洁、数组原语、符号表达力

```apl
⍝ APL: 一行搞定
(+/⍵)÷⍴⍵        ⍝ 平均值
⍳10              ⍝ 1..10
2 3⍴⍳6           ⍝ 2x3 矩阵
```

**Mora 继承**:
```mora
-- 不追求 APL 的符号密度，但追求表达力
-- 一行完成复杂操作
let avg = numbers | reduce(fn(a, b) = a + b, 0) / len(numbers)

-- range 生成序列
let nums = range(1, 11, 1)    -- [1, 2, ..., 10]

-- 模板字符串：高信息密度
let msg = p"Processed {len(items)} items in {elapsed}ms"

-- v0.91: 标量数学与统计（APL 数学原语全面落地）
let root = math.sqrt(2.0)
let mean = [1.0, 2.0, 3.0, 4.0, 5.0] | stats.mean     -- 3.0
let big = 99999999999999999999n                    -- BigInt 任意精度
```

| APL 原则 | Mora 体现 |
|----------|-----------|
| 数组原语 | `list` 方法（`map`/`filter`/`reduce`/`sort`/`sum`/`mean`/`stddev`/`histogram`） |
| 标量函数 | `math.*`（sin/cos/tan/exp/log/pow/sqrt/abs/floor/ceil + PI/E/TAU 常量） |
| 统计函数 | `stats.*`（mean/median/stddev/var/min/max/quantile/histogram/corr/cov） |
| 数值方法链 | `x.abs()` / `x.sqrt()` / `list.sum()` 等 Smalltalk 风格 |
| 线性代数 | `linalg.*`（dot/cross/norm/matmul/transpose） |
| 任意精度 | `123n` BigInt 字面量 + `Int + BigInt → BigInt` promotion |
| 表达力 | `p"..."` 模板、管道 |
| 简洁 | 3 行写完 HTTP 服务器 |
| 符号有意义 | `\|` 管道、`?` 错误传播、`p"..."` 模板 |

---

## 8. Logo — 教育性 + 最小语法

**基因**: 低门槛、渐进复杂度、可读性

```logo
; Logo: 教育语言，极简语法
TO SQUARE :SIZE
  REPEAT 4 [FD :SIZE RT 90]
END
```

**Mora 继承**:
```mora
-- 最小脚本：3 行
let name = "World"
print(p"Hello, {name}!")

-- 渐进复杂度：简单 → 复杂
-- Level 1: 直接执行
print("hello")

-- Level 2: 函数
task greet(name)
  print(p"Hello, {name}!")
end

-- Level 3: 类型 + trait
trait Greeter
  fn greet(self) -> string
end

-- Level 4: Agent 编排
let agent = ai.create("helper", {tools: ["search"]})
```

| Logo 原则 | Mora 体现 |
|-----------|-----------|
| 低门槛 | 无分号、无花括号、自然语言风格 |
| 渐进复杂度 | 可选类型注解、可选 trait |
| 可读性 | `task`/`let`/`for`/`if`/`end` |
| 教育友好 | 简单错误信息、REPL 即时反馈 |

---

## 9. Smalltalk — 纯 OOP + 消息传递

**基因**: 一切皆对象、消息传递、镜像

```smalltalk
"Smalltalk: 一切都是对象，一切都是消息"
3 + 4              "向 3 发送 + 消息"
Array new: 10      "向 Array 发送 new: 消息"
```

**Mora 继承**:
```mora
-- 方法调用：消息传递风格
let list = [1, 2, 3]
let upper = list | map(fn(x) = x * 2)   -- 向 list 发送 map 消息

-- 链式调用：连续消息传递
router
  | route("POST", "/api", handler)
  | listen("3000")

-- 类型即行为（trait 类似 Smalltalk 的协议）
trait Drawable
  fn draw(self) -> nil
end
```

| Smalltalk 原则 | Mora 体现 |
|----------------|-----------|
| 消息传递 | 方法调用 `\|` 管道 |
| 一切皆对象 | 所有值都有方法 |
| 镜像 | v1.0 计划（运行时反射） |
| 图像式环境 | REPL + 录制/重放 |

---

## 基因矩阵

| 语言 | 核心基因 | Mora 中的体现 |
|------|----------|---------------|
| **Clojure** | 不可变 | `let` 不可变、深拷贝赋值 |
| **Common Lisp** | 宏 | `with`/`route`/`observe` 语法块 |
| **Prolog** | 匹配 | `match` 语句、模式解构 |
| **Lisp** | 数据=代码 | `eval`/`quote`/`read`/quasiquote + `Value::Code` |
| **Ballerina** | 云原生 | `Router`/`McpServer`/`route` |
| **StreamIt** | 流 | `\|` 管道、`stream` 类型 |
| **APL** | 密度 | 管道链、模板字符串 |
| **Logo** | 简洁 | 无分号、自然语言风格 |
| **Smalltalk** | 消息 | 方法调用、链式管道 |

---

## Mora 的独特融合

```
Lisp 的灵魂    ──→  最小语法、代码即数据
  +
Smalltalk 的心  ──→  消息传递、一切皆对象
  +
Prolog 的脑    ──→  模式匹配、声明式
  +
Ballerina 的手  ──→  云原生、网络原语
  +
Clojure 的骨   ──→  不可变、函数式
  +
StreamIt 的血  ──→  流管道、数据流
  +
APL 的密度     ──→  表达力、符号
  +
Logo 的温度    ──→  低门槛、渐进复杂度
  =
Mora
```

**Mora = AI-native + 静态类型 + 云原生可观测 + 9 个语言的设计基因**

---

*2026-06-28*

---

## 10. 原语级借鉴（Primitive-level Influences, v0.75.10 核实）

语言级借鉴（1-9）之外，Mora 的**核心原语**直接映射到成熟系统/概念。
以下映射对照 `src/mir/handlers.rs` 等实现逐项核实（实现状态截至 v0.75.10）。

| Mora 原语 | 借鉴来源 | 核心机制 | 实现状态 |
|-----------|----------|----------|----------|
| `Transaction` / `Rollback` / `Commit` | 数据库 ACID | 补偿事务、undo log | ✅ 实现 — `h_transaction`（handlers.rs）：body 失败 → 执行 compensation → `Err("Transaction rolled back")`；`StepUndo`（pregel/mod.rs）为增量 undo log（Flink 增量 checkpoint 精神的本地形态） |
| `Send` / `Receive` | Actor Model / CSP | 异步消息传递、channel | ✅ 实现 — `h_send` 写入 dynamic_sends 缓冲 → ADVANCE 投递到 channel（`versions_seen` 版本快照做 delta 注入）；`h_receive` 从 channel 读 |
| `StreamFor` | 流处理 / 响应式编程 | 增量 token 生成 | ⚠️ 占位 — `h_stream_for` 仅隔离执行 body 一次；「增量 token 流」为设计意图，尚未实现 |
| `PromptSection` / `DocumentSection` | 文档系统 / 模板引擎 | 结构化内容生成 | ✅ 实现 — 隔离 child_env 执行 body（`h_prompt_section` / `h_document_section`） |
| `Eval` | 测试框架 / benchmark | 带容差的断言验证 | ✅ 实现 — `h_eval`：`tolerance: Option<f64>`，Float/Int 容差比较（\|g-e\| ≤ tol），失败 Err + 成功 `PASSED`（assert-with-epsilon） |
| `Observe` / `Span` | 可观测性 (OpenTelemetry) | 分布式追踪 | ✅ 实现 — `h_observe` / `h_span` 走 `run_isolated`（trace vars / span markers 副作用可见，v0.68 修复合并 child_env） |
| `SkillDef` / `Task` | 工作流编排 / RPA | 声明式技能定义 | ✅ 实现 — `h_skill_def`：name / description / version / requires / tasks / verify → `Dict` + `Value::Task`（`mir_body` 走 `execute`） |
| `Worker` / `Route` | 微服务 / 服务网格 | 工作单元路由 | ✅ 实现 — `Worker` = `run_isolated`（隔离工作单元）；`Route` 原语在 v0.75.81 删除（死代码），路由改由显式 API `Value::Router` + `.route()`/`.listen()` 方法承担 |

**核实结论**：8 项中 6 项与实现一致；`StreamFor`（增量 token 生成）与 `Route`（路由分发）为已定义语法、尚未实现的设计意图 — 列为后续候选。

---

*v0.75.10 addendum*

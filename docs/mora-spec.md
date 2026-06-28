# Mora 语言规范 (v0.20)

> **Mora** — AI-native 静态类型脚本语言，面向 Agent 编排与云原生可观测。
> 融合 9 个语言的设计基因：Prolog、StreamIt、APL、Clojure、Lisp、Smalltalk、Common Lisp、Ballerina、Logo。

---

## 目录

1. [哲学](#1-哲学)
2. [范式](#2-范式)
3. [类型](#3-类型)
4. [执行](#4-执行)
5. [内存](#5-内存)
6. [作用域](#6-作用域)
7. [控制流](#7-控制流)
8. [错误](#8-错误)
9. [并发](#9-并发)
10. [模块](#10-模块)
11. [元编程](#11-元编程)
12. [stdlib](#12-stdlib)
13. [形式化](#13-形式化)
14. [语法](#14-语法)
15. [相等性](#15-相等性)
16. [文本模型](#16-文本模型)
17. [安全](#17-安全)
18. [互操作](#18-互操作)
19. [工具链](#19-工具链)
20. [版本](#20-版本)

---

## 1. 哲学

### 1.1 设计目标

Mora 是一门为 **AI Agent 编排** 设计的语言，核心哲学：

| 原则 | 含义 |
|------|------|
| **AI-native** | `ai.chat`、`with`、`p"..."` 是语言一等公民，不是库 |
| **静态类型** | 编译期类型检查，但类型注解可选（类型推断） |
| **渐进复杂度** | 简单脚本 3 行搞定，复杂系统可扩展到 trait/泛型 |
| **云原生可观测** | `observe`/`span`/`record_tokens` 内置，零配置接入 OpenTelemetry |
| **可录制可重放** | `record`/`replay`/`diff` 语言级支持，Agent 行为可回归测试 |

### 1.2 设计哲学

```
简单的事简单做，复杂的事可以做。
```

- **不是** 通用编程语言（不追求图灵完备的花式玩法）
- **是** Agent 编排 DSL + 脚本语言的混合体
- 借鉴：Lua 的简洁、Rust 的类型安全、Python 的可读性、Erlang 的容错

### 1.3 命名

**Mora** = 拉丁语 "一小段时间"，暗示：
- 轻量（脚本级）
- 短生命周期（Agent 任务）
- 节奏感（pipeline/flow）

---

## 2. 范式

### 2.1 多范式融合

| 范式 | 体现 |
|------|------|
| **命令式** | `let`/`assign`/`for`/`if` |
| **函数式** | 闭包、管道 `\|>`、`map`/`filter`/`reduce` |
| **面向对象** | trait + impl + dyn dispatch |
| **声明式** | `route`/`observe`/`with` 块 |
| **数据流** | `p"..."` 模板、`stream for` |

### 2.2 一切皆表达式

大多数构造是表达式（有返回值）：

```mora
let x = if cond then "a" else "b" end
let y = match val with
  1 -> "one"
  _ -> "other"
end
let z = [1, 2, 3] |> map(fn(x) x * 2 end)
```

### 2.3 不可变优先

`let` 默认不可变。没有 `var` 关键字。要修改状态，用 `assign`（显式）：

```mora
let x = 10
assign x = 20    -- 显式修改
```

---

## 3. 类型

### 3.1 类型种类

| 类别 | 类型 | 语法 |
|------|------|------|
| **原语** | `string` | `"hello"` |
| | `char` | `'a'` |
| | `number` | `42`, `3.14` |
| | `bool` | `true`, `false` |
| | `nil` | `nil` |
| **容器** | `list<T>` | `[1, 2, 3]` |
| | `dict<K, V>` | `{a: 1, b: 2}` |
| **可调用** | `task` | `task name() ... end` |
| | `closure` | `fn(x) x + 1 end` |
| **AI** | `conversation` | `ai.create(...)` |
| | `stream` | `ai.stream(...)` |
| | `ai_config` | `with` 块内部 |
| | `ai_result` | `ai.chat(...)` 返回 |
| | `agent` | `agent.create(...)` |
| **HTTP** | `router` | `Router::new()` |
| | `http_request` | 框架内部 |
| | `http_response` | 框架内部 |
| **MCP** | `mcp_server` | `McpServer::new()` |
| **错误** | `result<T, E>` | `Ok(v)` / `Err(e)` |
| **类型系统** | `trait` | `trait Name ... end` |
| | `concrete` | `impl Trait for Type` |
| | `union` | `string \| number` |
| | `dyn Trait` | 动态分派 |
| **并发** | `atom` | `atom(0)` |
| | `compose` | `compose(f, g)` |
| | `partial` | `partial(fn, args)` |
| | `macro` | `macro name() ... end` |

### 3.2 类型推断

Mora 支持局部类型推断：

```mora
let x = 42          -- 推断为 number
let s = "hello"     -- 推断为 string
let l = [1, 2, 3]   -- 推断为 list<number>
```

函数参数和返回类型可选：

```mora
task add(a, b)      -- 参数类型推断
  return a + b
end

task greet(name: string): string   -- 显式注解（注意用 : 不是 ->）
  return p"Hello, {name}!"
end
```

### 3.3 泛型

```mora
trait Container<T>
  fn push(self, item: T) -> Self
  fn len(self) -> number
end

impl Container<T> for list<T>
  fn push(self, item: T) -> Self
    return self |> push(item)
  end

  fn len(self) -> number
    return len(self)
  end
end
```

### 3.4 Union 类型

```mora
task process(input: string | number): string
  return match input with
    s: string -> s.upper()
    n: number -> json.stringify(n)
  end
end
```

### 3.5 Trait 系统

```mora
trait Named
  fn name(self) -> string
end

trait Aged
  fn age(self) -> number
end

trait Person: Named, Aged    -- 继承
  fn greet(self) -> string
    return p"I'm {self.name()}, {self.age()} years old"
  end
end

impl Person for dict
  fn name(self) -> string
    return self.get("name")
  end

  fn age(self) -> number
    return self.get("age")
  end
end

let p: dyn Person = {"name": "Alice", "age": 30}
print(p.greet())
```

---

## 4. 执行

### 4.1 执行模型

Mora 是 **树遍历解释器**（tree-walking interpreter）：

1. **词法分析** → Token 流
2. **语法分析** → AST
3. **类型检查** → 编译期错误
4. **解释执行** → 树遍历求值

### 4.2 入口点

```mora
task main()
  print("Hello, Mora!")
end
```

`main` 是可选入口点。如果不存在，脚本从第一条语句开始执行。

### 4.3 求值顺序

- **从左到右**：函数参数、表达式操作数
- **严格求值**：所有参数在调用前求值

### 4.4 尾调用

当前版本 **不优化** 尾调用。深度递归可能导致栈溢出。

---

## 5. 内存

### 5.1 内存模型

Mora 使用 **引用计数 + 共享所有权**：

- `Value::List(Vec<Value>)` — 值类型，深拷贝
- `Value::Dict(HashMap<String, Value>)` — 值类型，深拷贝
- `Value::Closure { env: Arc<Mutex<Environment>> }` — 共享环境
- `Value::Router { routes: Arc<Mutex<Vec<...>>> }` — 共享可变状态

### 5.2 所有权语义

| 类型 | 赋值语义 |
|------|----------|
| `number`, `bool`, `char`, `nil` | 值拷贝 |
| `string` | 引用（不可变） |
| `list`, `dict` | **深拷贝**（值语义） |
| `closure`, `task` | 引用（共享环境） |
| `router`, `mcp_server` | 引用（共享可变状态） |
| `atom` | 引用（`Arc<Mutex<Value>>`） |

```mora
let a = [1, 2, 3]
let b = a           -- 深拷贝
assign b = [4, 5]   -- 不影响 a
```

**注意**: `list`/`dict` 的深拷贝为递归深拷贝；循环引用当前未检测，可能导致栈溢出。

### 5.3 垃圾回收

依赖 Rust 的 `Arc` 引用计数。无显式 GC。

---

## 6. 作用域

### 6.1 词法作用域

Mora 使用 **词法作用域**（静态作用域）：

```mora
let x = 10

task outer()
  let y = 20

  task inner()
    print(x + y)   -- 可以访问外层 x 和 y
  end

  inner()
end
```

### 6.2 作用域规则

| 构造 | 作用域 |
|------|--------|
| `let x = ...` | 当前块 |
| `task name() ... end` | 新作用域 |
| `fn(x) ... end` | 新作用域（捕获外层变量） |
| `with ...` | 嵌套作用域 |
| `for x in ...` | 新作用域 |
| `if ... then ... end` | 不创建新作用域（与外层共享） |

### 6.3 闭包捕获

闭包捕获 **引用**（通过 `Arc<Mutex<Environment>>`）：

```mora
let counter = 0

let inc = fn()
  assign counter = counter + 1
  return counter
end

print(inc())  -- 1
print(inc())  -- 2
```

### 6.4 变量遮蔽

`let` 可以遮蔽外层变量（创建新绑定）：

```mora
let x = 10
task test()
  let x = 20     -- 遮蔽外层 x
  print(x)       -- 20
end
test()
print(x)         -- 10（外层不受影响）
```

---

## 7. 控制流

### 7.1 条件

```mora
if condition then
  -- ...
else if other then
  -- ...
else
  -- ...
end
```

### 7.2 循环

```mora
-- for-in 循环
for item in list
  print(item)
end

-- range 循环
for i in range(0, 10, 1)
  print(i)
end

-- break / continue
for item in list
  if item == 3 then continue end
  if item == 7 then break end
  print(item)
end
```

### 7.3 模式匹配

```mora
let result = match value with
  0 -> "zero"
  1 -> "one"
  n: number -> p"other: {n}"
  s: string -> p"string: {s}"
  _ -> "unknown"
end
```

模式支持：
- 字面量：`0`, `"hello"`, `true`
- 变量绑定：`n: number`, `s: string`
- 通配符：`_`
- 列表解构：`[a, b, c]`
- 字典解构：`{name: n, age: a}`

### 7.4 守卫条件 (v0.16, Prolog 启发)

```mora
let result = match n with
  x when x > 0 -> "positive"
  x when x < 0 -> "negative"
  _ -> "zero"
end

-- 嵌套守卫
let result = match data with
  {age: age, name: name} when age >= 18 -> p"{name} is adult"
  {age: age, name: name} when age < 18 -> p"{name} is minor"
  _ -> "unknown"
end
```

### 7.5 列表 rest 模式 (v0.16, Prolog 启发)

```mora
-- Rest 模式
let [head, ...tail] = [1, 2, 3, 4]
-- head = 1, tail = [2, 3, 4]

-- match 中使用
match data with
  [head, ...tail] -> p"head={head}, tail={tail}"
  _ -> "empty"
end
```

### 7.6 管道

```mora
let result = "hello world"
  |> upper()
  |> split(" ")
  |> map(fn(w) w.trim() end)
  |> filter(fn(w) len(w) > 3 end)

-- v0.17: 管道支持闭包 (StreamIt 启发)
let double = fn(x) return x * 2 end
let result = 5 |> double    -- 10

-- 管道链
let result = 5 |> double |> add_one
```

### 7.7 Return

```mora
task early_return(x: number): string
  if x < 0 then
    return "negative"
  end
  return p"positive: {x}"
end
```

---

## 8. 错误

### 8.1 Result 类型

Mora 使用 `result<T, E>` 进行错误处理：

```mora
task divide(a: number, b: number): result<number, string>
  if b == 0 then
    return Err("division by zero")
  end
  return Ok(a / b)
end
```

### 8.2 ? 操作符

`?` 操作符用于错误传播：

```mora
task safe_divide(a: number, b: number): result<number, string>
  let result = divide(a, b)?
  return Ok(result)
end
```

等价于：

```mora
task safe_divide(a: number, b: number): result<number, string>
  let result = divide(a, b)
  match result with
    Ok(v) -> return Ok(v)
    Err(e) -> return Err(e)
  end
end
```

### 8.3 运行时错误

以下情况会触发运行时错误（panic）：
- 类型不匹配（运行时检查）
- 索引越界
- 未定义变量
- 网络请求失败（`web.fetch`）

### 8.4 错误处理最佳实践

```mora
-- 使用 match 处理 Result
let response = web.fetch("https://api.example.com")
match response with
  Ok(body) -> print(body)
  Err(e) -> print(p"Error: {e}")
end

-- 使用 ? 传播
task fetch_and_parse(url: string): result<dict, string>
  let body = web.fetch(url)?
  let data = json.parse(body)?
  return Ok(data)
end
```

---

## 9. 并发

### 9.1 Parallel 块

```mora
parallel
  let a = ai.chat("task 1")
  let b = ai.chat("task 2")
  let c = ai.chat("task 3")
end
```

### 9.2 Worker 并发 (v0.19, Ballerina 启发)

```mora
parallel
  worker w1
    print("worker 1 done")
  end

  worker w2
    print("worker 2 done")
  end
end
```

### 9.3 事务支持 (v0.19, Ballerina 启发)

```mora
-- 事务块
transaction
  print("in transaction")
  commit
end

-- 带补偿的事务
transaction
  print("in transaction")
  rollback
compensation
  print("compensating")
end
```

### 9.4 Atom 可变引用 (v0.19, Clojure 启发)

```mora
-- 原子引用（线程安全的可变状态）
let counter = atom(0)
swap(counter, fn(n) return n + 1 end)
swap(counter, fn(n) return n + 1 end)
let val = deref(counter)    -- 2
```

### 9.5 Agent 多步执行

Agent 使用 `max_steps` 限制执行步数：

```mora
let agent = ai.create("researcher", {
  tools: ["web_search", "summarize"],
  model: "gpt-4o",
  max_steps: 10,
  system: "You are a research assistant"
})

let result = agent.run("Find the latest news about AI")
```

---

## 10. 模块

### 10.1 Import

```mora
import "path/to/module.mora"
```

导入的模块在当前作用域中可用。

### 10.2 Export

```mora
export task helper()
  -- ...
end

export let CONFIG = {debug: true}
```

### 10.3 模块路径

- 相对路径：`import "./utils.mora"`
- 绝对路径：`import "/home/user/libs/utils.mora"`

### 10.4 内置模块

| 模块 | 说明 |
|------|------|
| `ai` | AI 原语 |
| `web` | HTTP 请求 |
| `json` | JSON 处理 |
| `file` | 文件系统 |
| `agent` | Agent 编排 |

---

## 11. 元编程

### 11.1 With 块

`with` 块用于设置上下文配置：

```mora
with model = "gpt-4o", temperature = 0.7
  let response = ai.chat("Hello")
end
```

支持的配置键：
- `model`: AI 模型名
- `system`: 系统提示词
- `temperature`: 温度
- `max_tokens`: 最大 token 数
- `budget`: Token 预算总量
- `per_call`: 单次调用 token 上限
- `mock_llm`: Mock 响应队列

### 11.2 模板字符串

```mora
let name = "Alice"
let greeting = p"Hello, {name}!"   -- "Hello, Alice!"
```

### 11.3 路由声明

```mora
route POST /api/chat -> handle_chat
route GET /api/health -> handle_health
```

### 11.4 可观测性

```mora
observe trace "my-service"
  span "request" tags {method: "POST", path: "/api"}
    -- ...
  end
end
```

### 11.5 用户自定义宏 (v0.20, Common Lisp 启发)

```mora
macro when(condition, body)
  if condition then
    body
  end
end

-- 使用
let x = 10
when(x > 5, print("big"))
```

**注意**: 当前宏为简单模板替换，无 hygiene；宏参数在调用者作用域中展开。

---

## 12. stdlib

### 12.1 内置函数

| 函数 | 签名 | 说明 |
|------|------|------|
| `print(x)` | `any -> nil` | 输出到 stdout |
| `range(start, end, step)` | `number, number, number -> list<number>` | 生成数值列表 |
| `len(x)` | `string \| list \| dict -> number` | 长度 |
| `compose(f1, f2, ...)` | `...closure -> compose` | 组合函数 (v0.18) |
| `partial(fn, args...)` | `closure, ...any -> partial` | 部分应用 (v0.18) |
| `atom(value)` | `any -> atom` | 创建可变引用 (v0.19) |
| `swap(atom, fn)` | `atom, closure -> any` | 原子更新 (v0.19) |
| `deref(atom)` | `atom -> any` | 读取引用值 (v0.19) |
| `type_of(value)` | `any -> string` | 返回类型名 (v0.20) |
| `is_instance(value, type)` | `any, string -> bool` | 类型检查 (v0.20) |
| `methods_of(value)` | `any -> list<string>` | 返回方法列表 (v0.20) |

### 12.2 String 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.len()` | `-> number` | 长度 |
| `.upper()` | `-> string` | 转大写 |
| `.lower()` | `-> string` | 转小写 |
| `.trim()` | `-> string` | 去空白 |
| `.starts_with(s)` | `string -> bool` | 前缀检查 |
| `.ends_with(s)` | `string -> bool` | 后缀检查 |
| `.contains(s)` | `string -> bool` | 包含检查 |
| `.split(sep)` | `string -> list<string>` | 分割 |
| `.replace(from, to)` | `string, string -> string` | 替换 |
| `.json()` | `-> result<dict, string>` | 解析 JSON |

### 12.3 List 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.push(item)` | `any -> list` | 追加（返回新列表） |
| `.pop()` | `-> list` | 弹出末尾（返回新列表） |
| `.get(i)` | `number -> any` | 按索引获取 |
| `.len()` | `-> number` | 长度 |
| `.map(fn)` | `closure -> list` | 映射 |
| `.filter(fn)` | `closure -> list` | 过滤 |
| `.reduce(fn, init)` | `closure, any -> any` | 归约 |
| `.take(n)` | `number -> list` | 取前 n 个元素 (v0.18) |
| `.drop(n)` | `number -> list` | 跳过前 n 个元素 (v0.18) |
| `.window(size)` | `number -> list` | 滑动窗口 (v0.17) |
| `.batch(size)` | `number -> list` | 批次处理 (v0.17) |
| `.shape()` | `-> list` | 返回维度 (v0.17) |
| `.flatten()` | `-> list` | 展平嵌套 (v0.17) |
| `.transpose()` | `-> list` | 转置二维列表 (v0.17) |
| `.reshape(rows, cols)` | `number, number -> list` | 重塑列表 (v0.17) |

### 12.3.1 广播算术 (v0.17, APL 启发)

Mora 支持列表的广播算术操作：

```mora
-- list * scalar: 每个元素乘以标量
[1, 2, 3] * 2       -- [2, 4, 6]

-- scalar + list: 标量加到每个元素
1 + [10, 20, 30]     -- [11, 21, 31]

-- list + list: 逐元素相加（等长）
[1, 2, 3] + [10, 20, 30]  -- [11, 22, 33]

-- list - list: 逐元素相减
[10, 20, 30] - [1, 2, 3]  -- [9, 18, 27]
```

支持的操作符：`+`, `-`, `*`, `/`, `%`

### 12.4 Dict 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.get(key)` | `string -> any` | 按键获取 |
| `.set(key, val)` | `string, any -> dict` | 设置键值（返回新字典） |
| `.keys()` | `-> list<string>` | 所有键 |
| `.values()` | `-> list` | 所有值 |
| `.len()` | `-> number` | 大小 |
| `.json()` | `-> result<dict, string>` | 解析 JSON |

### 12.5 AI 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `ai.chat(cfg, prompt)` | `ai_config, string -> ai_result` | AI 调用 |
| `ai.stream(prompt)` | `string -> stream` | 流式调用 |
| `ai.create(name, config)` | `string, dict -> agent` | 创建 Agent |
| `ai.critic(answer, ctx?)` | `string, string? -> value` | 评估输出 |

### 12.6 Web 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `web.fetch(url)` | `string -> result<string, string>` | HTTP GET |

### 12.7 JSON 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `json.parse(text)` | `string -> result<dict, string>` | 解析 JSON |
| `json.stringify(val)` | `any -> string` | 序列化 JSON |

### 12.8 File 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `file.read_text(path)` | `string -> string` | 读取文本 |
| `file.write_text(path, content)` | `string, string -> nil` | 写入文本 |
| `file.exists(path)` | `string -> bool` | 路径存在 |
| `file.list(path)` | `string -> list<string>` | 列出目录 |
| `file.mkdir_all(path)` | `string -> nil` | 递归创建目录 |
| `file.join(parts...)` | `...string -> string` | 路径拼接 |

完整 API 见 `file.read_bytes`、`file.write_bytes`、`file.append_text`、`file.remove`、`file.rename`、`file.copy`、`file.touch`、`file.is_file`、`file.is_dir`、`file.size`、`file.cwd`、`file.chdir`、`file.home_dir`、`file.abs`、`file.basename`、`file.dirname`、`file.extname`。

---

## 13. 形式化

### 13.1 类型系统形式化

Mora 的类型系统是 **结构化类型**（structural typing）+ **名义 trait**（nominal traits）：

```
τ ::= string | char | number | bool | nil
    | list<τ> | dict<τ, τ>
    | τ -> τ                    -- 函数类型
    | τ | τ                     -- union
    | result<τ, τ>
    | dyn T                     -- trait 对象
    | T<τ, ...>                 -- 泛型实例
```

### 13.2 类型判断规则

```
Γ ⊢ n : number                  (T-Num)
Γ ⊢ s : string                  (T-Str)
Γ ⊢ true : bool                 (T-True)

Γ ⊢ e₁ : number  Γ ⊢ e₂ : number
─────────────────────────────────────  (T-Add)
Γ ⊢ e₁ + e₂ : number

Γ ⊢ e : list<τ>  Γ ⊢ i : number
────────────────────────────────  (T-Index)
Γ ⊢ e[i] : τ

Γ ⊢ f : τ₁ -> τ₂  Γ ⊢ a : τ₁
────────────────────────────────  (T-Call)
Γ ⊢ f(a) : τ₂
```

### 13.3 Trait 检查规则

```
Γ ⊢ impl T for U    Γ ⊢ e : U
────────────────────────────────  (T-TraitDispatch)
Γ ⊢ e.method() : τ    where T defines method: ... -> τ
```

**待补充**: T-Let, T-If, T-Match, T-Pipe, T-Question 等判断规则。

---

## 14. 语法

### 14.1 词法

**关键字** (30+):

```
let task if then end return true false nil for in import export
parallel match with save load fn into as do read write append
read_bytes write_bytes stream tool break continue route observe
span tags record trace metrics otel trait impl dyn Self where
worker transaction commit rollback compensation macro
```

**运算符**:

```
+ - * / % = == != > < >= <= | |> -> ? ::
```

**分隔符**:

```
( ) [ ] { } . , :
```

### 14.2 EBNF 语法

```ebnf
program     = { statement } ;
statement   = let_stmt | assign_stmt | task_stmt | if_stmt | for_stmt
            | return_stmt | import_stmt | parallel_stmt | match_stmt
            | with_stmt | route_stmt | observe_stmt | trait_stmt
            | impl_stmt | worker_stmt | transaction_stmt | macro_stmt
            | expr_stmt ;

let_stmt    = "let" IDENTIFIER [ ":" type ] "=" expr ;
assign_stmt = "assign" IDENTIFIER "=" expr ;
task_stmt   = "task" IDENTIFIER "(" params ")" [ ":" type ] { statement } "end" ;
if_stmt     = "if" expr "then" { statement } { "else" "if" expr "then" { statement } } [ "else" { statement } ] "end" ;
for_stmt    = "for" IDENTIFIER "in" expr { statement } "end" ;
return_stmt = "return" [ expr ] ;
import_stmt = "import" STRING ;
match_stmt  = "match" expr "with" { pattern [ "when" expr ] "->" expr } "end" ;
with_stmt   = "with" bindings { statement } "end" ;
route_stmt  = "route" METHOD PATH "->" IDENTIFIER ;
worker_stmt = "worker" IDENTIFIER { statement } "end" ;
transaction_stmt = "transaction" { statement } [ "compensation" { statement } ] "end" ;
macro_stmt  = "macro" IDENTIFIER "(" params ")" { statement } "end" ;
expr_stmt   = expr ;

expr        = literal | variable | binary | call | method_call
            | index | closure | prompt | pipe | question ;

literal     = NUMBER | STRING | CHAR | BOOL | NIL | list_literal | dict_literal ;
list_literal = "[" [ expr { "," expr } [ "..." IDENTIFIER ] ] "]" ;
dict_literal = "{" [ IDENTIFIER ":" expr { "," IDENTIFIER ":" expr } ] "}" ;

prompt      = "p" STRING ;    -- 模板字符串
closure     = "fn" "(" params ")" ( expr | "{" { statement } "}" ) ;
pipe        = expr "|>" expr ;
question    = expr "?" ;
```

### 14.3 注释

```mora
-- 单行注释
```

无多行注释。

### 14.4 标识符

- 以字母或下划线开头
- 后续字符可以是字母、数字、下划线
- 大小写敏感
- 无保留字冲突（关键字已从标识符中分离）

---

## 15. 相等性

### 15.1 值相等 (`==`)

| 类型 | 比较方式 |
|------|----------|
| `number` | 数值相等 |
| `string` | 字符串内容相等 |
| `char` | 字符相等 |
| `bool` | 布尔值相等 |
| `nil` | 总是相等 |
| `list` | **深比较**（递归比较每个元素） |
| `dict` | **深比较**（递归比较每个键值对） |
| 其他 | 引用相等（指针比较） |

```mora
[1, 2, 3] == [1, 2, 3]     -- true
{a: 1} == {a: 1}           -- true
nil == nil                  -- true
```

### 15.2 引用相等

无显式引用相等运算符。对于闭包、路由器等引用类型，`==` 比较引用。

### 15.3 不等性 (`!=`)

`!=` 是 `==` 的逻辑取反。

---

## 16. 文本模型

### 16.1 字符串

- 使用双引号：`"hello"`
- 转义序列：`\n`, `\t`, `\\`, `\"`
- 无字符数组概念（字符串是不可变的）

### 16.2 模板字符串

```mora
let name = "Alice"
let age = 30
let msg = p"Hello, {name}! You are {age} years old."
```

花括号 `{}` 内可以是任意表达式：

```mora
let x = 10
let msg = p"Result: {x * 2}"
```

### 16.3 字符

```mora
let c = 'a'
let newline = '\n'
```

### 16.4 JSON

```mora
let data = json.parse('{"name": "Alice"}')
let text = json.stringify({name: "Alice"})
```

---

## 17. 安全

### 17.1 沙箱

当前版本 **无沙箱**。脚本可以：
- 读写文件系统
- 发送网络请求

v1.0 计划：
- 权限系统（类似 Deno）
- 文件系统访问白名单
- 网络域名白名单

### 17.2 Secret 处理

- `record` 命令自动脱敏（`redact_secrets`）
- `.moraignore` 策略文件控制脱敏规则
- `mora audit` 检测潜在 secret 泄露

### 17.3 Token 预算

```mora
with budget = 10000, per_call = 1000
  -- 超出预算会报错
end
```

---

## 18. 互操作

### 18.1 HTTP 服务器

```mora
let router = Router::new()

let handle_chat = fn(req)
  let body = req.body.json()
  let response = ai.chat(body.get("prompt"))
  return {status: 200, body: json.stringify({reply: response})}
end

router
  |> route("POST", "/api/chat", handle_chat)
  |> listen("0.0.0.0:3000")
```

### 18.2 MCP 服务器

```mora
let server = McpServer::new()

let greet_handler = fn(params)
  return p"Hello, {params.get("name")}!"
end

server
  |> tool("greet", {name: {type: "string"}}, greet_handler)
  |> serve()
```

### 18.3 HTTP 客户端

```mora
let response = web.fetch("https://api.example.com/data")
let data = json.parse(response)
```

### 18.4 JSON 互操作

```mora
-- 序列化
let json_str = json.stringify({name: "Alice", age: 30})

-- 反序列化
let result = json.parse(json_str)
match result with
  Ok(data) -> print(data.get("name"))
  Err(e) -> print(p"Parse error: {e}")
end
```

---

## 19. 工具链

### 19.1 CLI

```bash
mora <file.mora>              # 运行脚本
mora --repl                   # 交互式 REPL
mora --check <file>           # 仅类型检查
mora --version                # 版本
mora --help                   # 帮助
```

### 19.2 录制/重放

```bash
mora record <file> <name>     # 录制 AI 调用
mora replay <file> <name>     # 重放（确定性）
mora diff <name-a> <name-b>   # 对比两次运行
mora record list              # 列出所有录制
mora record stats <name>      # 统计汇总
mora record timeline <name>   # 调用时间线
mora record export <name>     # 导出 JSONL/Markdown
mora record audit <name>      # 脱敏扫描
mora record report <name>     # 生成证据报告
mora snapshot <file> <name>   # 快照测试
```

### 19.3 LSP

Mora 提供 Language Server Protocol 支持：

- 诊断（类型错误、语法错误）
- 悬停信息
- 代码补全（计划中）
- 跳转定义（计划中）

### 19.4 Mock LLM

```mora
with mock_llm = ["response 1", "response 2"]
  let r1 = ai.chat("first")   -- 返回 "response 1"
  let r2 = ai.chat("second")  -- 返回 "response 2"
end
```

---

## 20. 版本

### 20.1 版本语义

Mora 使用 **语义化版本**（Semantic Versioning）：

```
v0.x.y  -- 0.x 阶段，可能有 breaking change
v1.0.0  -- 首个稳定版本
```

### 20.2 当前版本

**v0.20** — 9 语言特性融入 (16 个特性)

### 20.3 版本历史

| 版本 | 特性 |
|------|------|
| v0.01 | 基础解释器 |
| v0.04 | AI 原语 (`ai.chat`, `with`, `p"..."`) |
| v0.06 | HTTP 服务器 (`Router::new()`) |
| v0.07 | MCP 服务器 (`McpServer::new()`) |
| v0.08 | Trait 系统 |
| v0.09 | 泛型 |
| v0.10 | AI retry + exponential backoff |
| v0.11 | HTTP 端口冲突修复 |
| v0.12 | MCP stdio transport |
| v0.13 | 删除 `Type::Any` + Walrus 语法 |
| v0.14 | record/replay/diff CLI |
| v0.15 | AI config 接入 + record 扩展 + snapshot + mock_llm |
| v0.16 | match 守卫条件 + 列表 ...rest + Dict 部分匹配 (Prolog) |
| v0.17 | 管道闭包 + window/batch + 数组操作 + 广播 (StreamIt/APL) |
| v0.18 | compose + take/drop + partial (Clojure/Lisp) |
| v0.19 | Worker 并发 + 事务 + atom/swap/deref (Ballerina/Clojure) |
| v0.20 | 运行时反射 + 宏 + 重构 (Smalltalk/Common Lisp) |

### 20.4 兼容性

- v0.x 版本之间 **不保证** 向后兼容
- v1.0 后严格遵循语义化版本
- breaking change 只在主版本号变更时引入

### 20.5 版本路线图

| 版本 | 状态 | 特性 |
|------|------|------|
| v0.16 | ✅ 已发布 | match 守卫条件 + 列表 ...rest 模式 + Dict 部分匹配 |
| v0.17 | ✅ 已发布 | 管道闭包 + window/batch + 数组操作 + 广播算术 |
| v0.18 | ✅ 已发布 | compose + take/drop + partial |
| v0.19 | ✅ 已发布 | Worker 并发 + 事务 + atom/swap/deref |
| v0.20 | ✅ 已发布 | 运行时反射 + 宏 + 重构 (value.rs/flow.rs) |
| v0.21 | 🔄 计划中 | `.moraignore` 完善 + `mora audit` 增强 |
| v0.22 | 🔄 计划中 | `with mock_llm` 增强 + snapshot 语义对比 |
| v0.23 | 🔄 计划中 | 异步 I/O (`async/await`) |
| v0.24 | 🔄 计划中 | 沙箱 + 权限系统 |
| v1.0 | 📌 目标 | 稳定 API + 完整 stdlib |

---

## 附录

### A. 关键字完整列表

```
let task if then end return true false nil for in import export
parallel match with save load fn into as do read write append
read_bytes write_bytes stream tool break continue route observe
span tags record trace metrics otel trait impl dyn Self where
worker transaction commit rollback compensation macro
```

### B. 内置类型完整列表

```
string char number bool nil list dict task closure conversation
stream ai_config ai_result ai_error ai_module router http_request
http_response mcp_server result any trait concrete union
compose partial atom macro
```

### C. 运算符优先级

| 优先级 | 运算符 |
|--------|--------|
| 1 (高) | `()` `[]` `.` `::` |
| 2 | `!` `-` (一元) |
| 3 | `*` `/` `%` |
| 4 | `+` `-` |
| 5 | `==` `!=` `<` `>` `<=` `>=` |
| 6 | `\|>` (管道) |
| 7 | `?` |
| 8 (低) | `=` |

### D. 文件扩展名

- `.mora` — Mora 源文件
- `.snap.jsonl` — 快照基线文件
- `.jsonl` — 录制文件
- `.moraignore` — 脱敏策略文件

---

*Mora 语言规范 v0.20 — 2026-06-28*

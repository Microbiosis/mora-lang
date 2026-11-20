# Mora 语言特性融入计划 (v2)

> 从 9 个语言中学习 27 个特性，分 6 个阶段融入。
> 基于 MCP 搜索结果更新，包含具体实现参考。

---

## 总览

| 阶段 | 主题 | 特性数 | 版本 | 来源 | 状态 |
|------|------|--------|------|------|------|
| P1 | 模式匹配增强 | 3 | v0.16 | Prolog | ✅ 完成 |
| P2 | 管道与流 | 4 | v0.17 | StreamIt + APL | ✅ 完成 |
| P3 | 函数式核心 | 3 | v0.18 | Clojure + Lisp | ✅ 完成 |
| P4 | 并发与事务 | 3 | v0.19 | Clojure + Ballerina | 部分完成 (P4.1 ✅) |
| P5 | 反射与元编程 | 3 | v0.20 | Smalltalk + Common Lisp | 部分完成 (P5.1, P5.2 ✅) |
| P6 | 远期探索 | 11 | v1.0+ | 9 语言综合 | 待定 |

**测试增量**: 147 → 178 (+31 tests)

---

## P1: 模式匹配增强 (v0.16)

**来源**: Prolog

### 1.1 Match 守卫条件

**参考**: SWI-Prolog 的 `=>` 规则和 Guard 语法（[SWI-Prolog SSU](https://www.swi-prolog.org/pldoc/man?section=ssu)）

**Mora 语法**:
```mora
let result = match value
  | n when n > 0 => "positive"
  | n when n < 0 => "negative"
  | _ => "zero"
end

-- 嵌套守卫
let result = match data
  | {name, age} when age >= 18 and name != "" => p"Adult: {name}"
  | {name, age} when age < 18 => p"Minor: {name}"
  | _ => "invalid"
end
```

**实现要点**:
- `ast.rs`: `Pattern` 枚举增加 `Guard { pattern: Box<Pattern>, condition: Expr }`
- `parser.rs`: 在 `|` 后解析 pattern，然后检查 `when` 关键字
- `interpreter.rs`: 匹配成功后求值守卫条件，必须返回 bool
- `typeck.rs`: 守卫条件类型检查

### 1.2 列表解构增强

**参考**: Haskell/Prolog 的列表模式匹配

**Mora 语法**:
```mora
-- Rest 模式
let [head, ...tail] = [1, 2, 3, 4]
-- head = 1, tail = [2, 3, 4]

-- 忽略剩余
let [a, b, ..] = [1, 2, 3, 4]
-- a = 1, b = 2

-- 嵌套解构
let [x, [y, z]] = [1, [2, 3]]
-- x = 1, y = 2, z = 3
```

**实现要点**:
- `ast.rs`: `Pattern::List` 增加 `Rest` 变体
- `lexer.rs`: 新增 `...` 和 `..` token
- `parser.rs`: 列表模式中解析 `...`/`..`
- `interpreter.rs`: 列表解构时处理剩余元素

### 1.3 Dict 解构增强

**Mora 语法**:
```mora
-- 忽略多余字段
let {name, age, ..} = {"name": "Alice", "age": 30, "city": "NYC"}
-- name = "Alice", age = 30

-- 重命名
let {name: n, age: a} = {"name": "Alice", "age": 30}
-- n = "Alice", a = 30
```

---

## P2: 管道与流 (v0.17)

**来源**: StreamIt + APL

### 2.1 管道增强

**参考**: StreamIt 的 Pipeline/Filter/Joiner 结构（[StreamIt Language Spec](https://groups.csail.mit.edu/cag/streamit/shtml/documentation.shtml)）

**Mora 语法**:
```mora
-- 管道支持任意表达式（不只是方法调用）
let result = data
  | transform(x)
  | validate
  | save_to_db

-- 管道组合器
let pipeline = compose(transform, validate, save)
let result = pipeline(data)
```

**实现要点**:
- `interpreter.rs`: `evaluate_pipe` 支持任意右侧表达式
- 新增 `compose` 内置函数：`compose(f, g, h)` 返回 `fn(x) = h(g(f(x)))`

### 2.2 窗口聚合

**参考**: StreamIt 的 Window 操作符

**Mora 语法**:
```mora
-- 滑动窗口
let avg = stream
  | window(5)
  | map(fn(w) = reduce(w, fn(a, b) = a + b, 0) / len(w))

-- 翻转窗口（批次处理）
let batches = data | batch(100)

-- 带步长的窗口
let windows = data | window(5, step=2)
```

**实现要点**:
- 新增 `window(size, step?)` 和 `batch(size)` 内置方法
- 返回 `list<list<T>>` 类型
- 实现为惰性迭代器（避免大数据复制）

### 2.3 数组操作

**参考**: APL 的 `⍴` (Reshape), `⍉` (Transpose), `,` (Ravel)（[APL Wiki: Reshape](https://www.aplwiki.com/wiki/Reshape), [Transpose](https://www.aplwiki.com/wiki/Transpose)）

**Mora 语法**:
```mora
-- 二维列表
let matrix = [[1, 2], [3, 4], [5, 6]]

-- Shape
let shape = matrix.shape()        -- [3, 2]

-- Flatten
let flat = matrix.flatten()       -- [1, 2, 3, 4, 5, 6]

-- Transpose
let transposed = matrix.transpose()  -- [[1, 3, 5], [2, 4, 6]]

-- Reshape
let reshaped = matrix.reshape(2, 3)  -- [[1, 2, 3], [4, 5, 6]]
```

**实现要点**:
- `Value::List` 支持嵌套（二维列表）
- 新增 `.shape()`, `.flatten()`, `.transpose()`, `.reshape()` 方法
- Reshape 逻辑：元素按 ravel 顺序复制，不足则循环重复

### 2.4 广播操作

**参考**: APL 的逐元素操作和 NumPy 广播规则

**Mora 语法**:
```mora
let a = [1, 2, 3]
let b = [10, 20, 30]

-- 逐元素运算
let c = a + b     -- [11, 22, 33]

-- 标量广播
let d = a * 2     -- [2, 4, 6]

-- 归约操作
let sum = a | reduce(fn(acc, x) = acc + x, 0)  -- 6
```

**实现要点**:
- 算术运算符对 `list` 类型重载
- 标量自动广播到 list
- 错误：长度不匹配时报错

---

## P3: 函数式核心 (v0.18)

**来源**: Clojure + Lisp

### 3.1 Transducer

**参考**: Clojure Transducers（[Clojure Transducers](https://clojure.org/reference/transducers), [Grokking Transducers](https://dev.solita.fi/2021/10/14/grokking-clojure-transducers.html)）

**核心概念**: Transducer 是独立于上下文的变换组合器，可以跨 collection、channel、stream 复用。

**Mora 语法**:
```mora
-- 定义 transducer（用 comp 组合）
let xf = compose(
  filter(fn(x) = x > 0),
  map(fn(x) = x * 2),
  take(5)
)

-- 应用到不同上下文
let list_result = [1, -2, 3, -4, 5, 6, 7] | into([], xf)
-- [2, 6, 10, 12, 14]

-- 应用到 stream
let stream_result = ai.stream("...") | into("", xf)
```

**实现要点**:
- 新增 `Value::Transducer` 变体
- `compose(fns...)` 创建 transducer
- `into(collection, xf)` 应用 transducer
- 内部实现：每个变换是一个 `reducing function -> reducing function` 的函数

### 3.2 惰性序列

**参考**: Clojure 的 LazySeq

**Mora 语法**:
```mora
-- 惰性序列（按需求值）
let nums = lazy_range(0, 1000000)
let first_10 = nums | take(10)   -- 只计算前 10 个

-- 无限序列
let naturals = lazy_range(0, infinity)
let first_100 = naturals | take(100)
```

**实现要点**:
- 新增 `Value::LazySeq { generator: Closure, state: Value }`
- `lazy_range(start, end?)` 创建惰性序列
- `take(n)`, `drop(n)` 操作惰性序列
- 按需求值，避免大内存分配

### 3.3 部分应用与柯里化

**参考**: Clojure 的 `#()` 和 `partial`，Racket 的 `curry`

**Mora 语法**:
```mora
-- 占位符部分应用
let add = fn(a, b) = a + b
let add5 = add(5, _)
print(add5(3))   -- 8

-- 多占位符
let greet = fn(greeting, name) = p"{greeting}, {name}!"
let hello = greet("Hello", _)
print(hello("Alice"))   -- "Hello, Alice!"

-- 柯里化
let curried_add = fn(a) = fn(b) = a + b
print(curried_add(5)(3))   -- 8

-- partial 函数
let add10 = partial(add, 10)
print(add10(5))   -- 15
```

**实现要点**:
- `_` 作为占位符，返回闭包
- `partial(fn, args...)` 内置函数
- 自动柯里化：参数不足时返回闭包

---

## P4: 并发与事务 (v0.19)

**来源**: Clojure + Ballerina

### 4.1 Atom 可变引用

**参考**: Clojure 的 Atom（[Clojure Reference: Atoms](https://clojure.org/reference/atoms)）

**Mora 语法**:
```mora
-- 原子引用（线程安全的可变状态）
let counter = atom(0)
swap(counter, fn(n) = n + 1)
print(deref(counter))   -- 1

-- CAS 操作
let success = compare_and_set(counter, 1, 10)
print(deref(counter))   -- 10 (if success)

-- 监听变化
watch(counter, "logger", fn(key, old, new)
  print(p"changed from {old} to {new}")
end)

-- 移除监听
unwatch(counter, "logger")
```

**实现要点**:
- 新增 `Value::Atom { value: Arc<Mutex<Value>>, watchers: Arc<Mutex<HashMap<String, Closure>>> }`
- `atom(value)`, `swap(atom, fn)`, `deref(atom)`, `compare_and_set(atom, old, new)`
- `watch(atom, key, fn)`, `unwatch(atom, key)`
- 基于 `Arc<Mutex<>>` 实现线程安全

### 4.2 Worker 并发

**参考**: Ballerina 的 Worker 和消息传递（[Ballerina Workers](https://ballerina.io/learn/by-example/)）

**Mora 语法**:
```mora
parallel
  worker w1
    let result1 = ai.chat("task 1")
    -> result1 to main
  end

  worker w2
    let result2 = web.fetch("https://api.example.com")
    -> result2 to main
  end

  worker main
    let r1 = <- w1
    let r2 = <- w2
    print(p"Results: {r1}, {r2}")
  end
end
```

**实现要点**:
- `parallel` 块支持 `worker` 声明
- `-> value to worker` 发送消息
- `let x = <- worker` 接收消息
- 基于 `channel` 实现 worker 间通信
- 超时机制：`<- worker timeout 5000`

### 4.3 事务支持

**参考**: Saga 模式（[Microservices.io: Saga](https://microservices.io/patterns/data/saga.html)）

**Mora 语法**:
```mora
-- 事务块
transaction
  let order = create_order(data)
  let payment = process_payment(order)
  if payment.error then
    rollback
  end
  commit
end

-- 带补偿的事务
transaction
  let order = create_order(data)
  compensation
    cancel_order(order.id)
  end

  let payment = process_payment(order)
  compensation
    refund_payment(payment.id)
  end
  commit
end
```

**实现要点**:
- 新增 `transaction`/`commit`/`rollback` 关键字
- `compensation` 块注册补偿操作
- 基于 saga 模式：失败时按逆序执行补偿
- 嵌套事务支持

---

## P5: 反射与元编程 (v0.20)

**来源**: Smalltalk + Common Lisp

### 5.1 运行时反射

**参考**: Smalltalk 的反射 API（[Reflective Facilities in Smalltalk-80](https://www.laputan.org/ref89/ref89.html)）

**Mora 语法**:
```mora
-- 类型检查
let x = 42
print(type_of(x))              -- "number"
print(is_instance(x, number))   -- true

-- 方法列表
let methods = methods_of(x)
print(methods)   -- ["+", "-", "*", "/", ...]

-- 动态调用
let result = invoke(x, "+", [10])   -- 52

-- 类型名
print(type_name(x))   -- "number"
```

**实现要点**:
- `type_of(value)`, `type_name(value)` 返回类型字符串
- `is_instance(value, type)` 类型检查
- `methods_of(value)` 返回方法名列表
- `invoke(object, method, args)` 动态调用

### 5.2 消息链

**参考**: Smalltalk 的 Cascaded Messages（[Cascading Messages in Smalltalk](https://donraab.medium.com/cascading-messages-in-smalltalk-14807389b6ce)）

**Mora 语法**:
```mora
-- 级联消息（对同一接收者连续调用）
let router = Router::new()
router
  ; route("POST", "/api/chat", handle_chat)
  ; route("GET", "/api/health", handle_health)
  ; listen("0.0.0.0:3000")

-- 等价于：
let r1 = router.route("POST", "/api/chat", handle_chat)
let r2 = r1.route("GET", "/api/health", handle_health)
let r3 = r2.listen("0.0.0.0:3000")
```

**实现要点**:
- 新增 `;` 操作符（级联消息）
- 语义：对同一接收者连续调用，每步返回接收者本身
- 实现：`a ; b ; c` → `let _1 = a; _1.b; _1.c; _1`

### 5.3 用户自定义宏

**参考**: Common Lisp 的 `defmacro`（[Common Lisp Macros By Example](https://lisp-journey.gitlab.io/blog/common-lisp-macros-by-example-tutorial/)）

**Mora 语法**:
```mora
-- 宏定义（编译期 AST 变换）
macro when(condition, body)
  return if condition then body else nil end
end

-- 使用
when(x > 10, print("big"))

-- 宏展开调试
macroexpand(when(x > 10, print("big")))
-- => if x > 10 then print("big") else nil end
```

**实现要点**:
- 新增 `macro` 关键字
- 宏在编译期展开，操作 AST
- 卫生宏：自动变量重命名避免冲突
- `macroexpand(expr)` 调试宏展开

---

## P6: 远期探索 (v1.0+)

| 特性 | 来源 | 参考 |
|------|------|------|
| Turtle Graphics | Logo | [Logo Primer](https://el.media.mit.edu/logo-foundation/what_is_logo/logo_primer.html) |
| 图像式持久化 | Smalltalk | Smalltalk-80 image |
| Reader 宏 | Lisp | Racket `curly-fn` |
| Condition/Restart | Common Lisp | CLHS Condition System |
| CLOS 对象系统 | Common Lisp | CLOS |
| 回溯搜索 | Prolog | SWI-Prolog |
| Split/Join 流 | StreamIt | StreamIt Spec |
| 高维数组 | APL | [APL Wiki](https://aplwiki.com/) |
| STM | Clojure | Clojure Refs |
| 交互式可视化 | Logo | Turtle Academy |
| Quote/Unquote | Lisp | Lisp quasiquote |

---

## 实现优先级矩阵

| 特性 | 影响力 | 复杂度 | 优先级 | 来源 |
|------|--------|--------|--------|------|
| Match 守卫条件 | 高 | 低 | P1 | Prolog |
| 列表/Dict 解构 | 高 | 低 | P1 | Prolog |
| 管道增强 | 高 | 中 | P2 | StreamIt |
| 窗口聚合 | 中 | 中 | P2 | StreamIt |
| 数组操作 | 中 | 中 | P2 | APL |
| 广播操作 | 中 | 高 | P2 | APL |
| Transducer | 高 | 高 | P3 | Clojure |
| 惰性序列 | 中 | 中 | P3 | Clojure |
| 部分应用 | 中 | 低 | P3 | Lisp |
| Atom 可变引用 | 高 | 中 | P4 | Clojure |
| Worker 并发 | 高 | 高 | P4 | Ballerina |
| 事务支持 | 中 | 高 | P4 | Ballerina |
| 运行时反射 | 高 | 中 | P5 | Smalltalk |
| 消息链 | 低 | 低 | P5 | Smalltalk |
| 用户自定义宏 | 高 | 高 | P5 | Common Lisp |

---

## 实现状态 (v0.20)

| 特性 | 来源 | 测试 | 文件 | 状态 |
|------|------|------|------|------|
| match 守卫条件 | Prolog | +3 | ast/parser/interpreter/typeck | ✅ |
| 列表 ...rest 模式 | Prolog | +3 | ast/parser/lexer/interpreter | ✅ |
| Dict 部分匹配 | Prolog | +2 | interpreter | ✅ |
| 管道支持闭包 | StreamIt | +2 | interpreter | ✅ |
| window/batch | StreamIt | +2 | interpreter | ✅ |
| shape/flatten/transpose/reshape | APL | +4 | interpreter | ✅ |
| 广播算术 | APL | +3 | interpreter | ✅ |
| compose | Clojure | +2 | interpreter | ✅ |
| take/drop | Clojure | +2 | interpreter | ✅ |
| partial | Lisp | +2 | interpreter | ✅ |
| atom/swap/deref | Clojure | +2 | interpreter | ✅ |
| Worker 并发 | Ballerina | +1 | ast/lexer/parser/interpreter | ✅ |
| 事务支持 | Ballerina | +2 | ast/lexer/parser/interpreter | ✅ |
| type_of/is_instance/methods_of | Smalltalk | +3 | interpreter | ✅ |
| 消息链 (管道链) | Smalltalk | +1 | interpreter | ✅ |
| 用户自定义宏 | Common Lisp | +1 | ast/lexer/parser/interpreter | ✅ |

**总计**: 16 个特性, +35 tests, 147 → 182

## 参考链接

| 语言 | 核心参考 |
|------|----------|
| Clojure | [Data Structures](https://clojure.org/reference/data_structures), [Transducers](https://clojure.org/reference/transducers) |
| Common Lisp | [Macros Tutorial](https://lisp-journey.gitlab.io/blog/common-lisp-macros-by-example-tutorial/) |
| Prolog | [SWI-Prolog SSU](https://www.swi-prolog.org/pldoc/man?section=ssu) |
| StreamIt | [Language Spec](https://groups.csail.mit.edu/cag/streamit/shtml/documentation.shtml) |
| APL | [Reshape](https://www.aplwiki.com/wiki/Reshape), [Transpose](https://www.aplwiki.com/wiki/Transpose) |
| Smalltalk | [Cascading Messages](https://donraab.medium.com/cascading-messages-in-smalltalk-14807389b6ce), [Reflection](https://www.laputan.org/ref89/ref89.html) |
| Logo | [Logo Primer](https://el.media.mit.edu/logo-foundation/what_is_logo/logo_primer.html) |
| Ballerina | [Saga Pattern](https://microservices.io/patterns/data/saga.html) |

---

*2026-06-28 v3 — 13 个特性实现完成*

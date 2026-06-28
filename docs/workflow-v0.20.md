# Mora 版本演进工作流 (v0.11 → v0.20)

> 从 v0.11 到 v0.20 的完整版本演进记录，每个版本的开发流程和衔接。

---

## 版本演进总览

```
v0.11 (HTTP 端口冲突处理)
  ↓ 删除旧语法
v0.12 (MCP stdio transport)
  ↓ 激进清理
v0.13 (删 Type::Any + Walrus 语法)
  ↓ 录制重放
v0.14 (record/replay/diff CLI)
  ↓ AI config 接入
v0.15 (AI config + record 扩展)
  ↓ 模式匹配增强
v0.16 (match 守卫 + ...rest + Dict)
  ↓ 管道与流
v0.17 (管道闭包 + 数组操作 + 广播)
  ↓ 函数式核心
v0.18 (compose/take/drop/partial)
  ↓ 并发与事务
v0.19 (Worker + 事务 + atom)
  ↓ 反射与元编程
v0.20 (反射 + 宏 + 重构)
```

---

## v0.11: HTTP 端口冲突处理

### 目标
- 解决 HTTP server 端口被占用的问题
- 实现 SO_REUSEADDR + 自动选端口

### 开发流程
```bash
# 1. 检查当前状态
cargo build && cargo test

# 2. 实现功能
# src/http_server.rs: 添加端口冲突处理

# 3. 测试
cargo test

# 4. 提交
git commit -m "v0.11: HTTP server 端口冲突处理"
```

### 产出
- HTTP server 启动时自动尝试 4 个端口
- 设置 SO_REUSEADDR

---

## v0.12: MCP stdio transport

### 目标
- 支持 MCP 协议的 stdio 传输

### 开发流程
```bash
# 1. 实现 MCP stdio
# src/mcp_server.rs: 添加 stdio transport

# 2. 测试
cargo test

# 3. 提交
git commit -m "v0.12: MCP stdio transport"
```

---

## v0.13: 激进清理

### 目标
- 删除 Type::Any 变体
- 删除 Walrus 语法 (:=)

### 开发流程
```bash
# 1. 删除 Type::Any
# src/typeck.rs: 移除 Any 变体

# 2. 删除 Walrus 语法
# src/parser.rs: 移除 := 解析

# 3. 修复所有编译错误
cargo build 2>&1 | grep "error"

# 4. 测试
cargo test

# 5. 提交
git commit -m "feat(typeck): v0.13 激进档 - 删 Type::Any 变体 + 删 Walrus 语法 (breaking)"
```

---

## v0.14: record/replay/diff CLI

### 目标
- 实现 AI 调用录制/重放/对比
- 受 FlightBox 启发

### 开发流程
```bash
# 1. 创建 record.rs 模块
# src/record.rs: 实现 Recorder, Event, load/save

# 2. 添加 CLI 命令
# src/main.rs: 添加 record/replay/diff 子命令

# 3. 集成到 interpreter
# src/interpreter.rs: 在 ai.chat 中录制

# 4. 测试
cargo test

# 5. 提交
git commit -m "feat(record): v0.14 record/replay/diff CLI"
```

### 产出
```bash
mora record script.mora demo-001   # 录制
mora replay script.mora demo-001   # 重放
mora diff demo-001 demo-002        # 对比
```

---

## v0.15: AI config + record 扩展

### 目标
- 接入 5 个遗留 TODO
- 扩展 record CLI (list/stats/timeline/export/audit/report/snapshot/mock_llm)

### 开发流程
```bash
# 1. 检查 TODO
grep -rn "TODO" src/ --include="*.rs"

# 2. 接入 TokenBudget.per_call
# src/interpreter.rs: track_tokens 检查

# 3. 接入 AiConfig (max_tokens/system/temperature)
# src/interpreter.rs: real_ai_chat_with_tools 读取

# 4. 接入 mock_llm
# src/interpreter.rs: with mock_llm = [...]

# 5. 扩展 record CLI
# src/record.rs: list_recordings, compute_stats, build_timeline
# src/main.rs: 添加子命令

# 6. 测试
cargo test

# 7. 提交
git commit -m "feat(v0.15): AI config 接入 + record CLI 扩展"
```

### 产出
```bash
mora record list              # 列出所有录制
mora record stats <name>      # 统计汇总
mora record timeline <name>   # 调用时间线
mora record export <name>     # 导出 JSONL/Markdown
mora record audit <name>      # 脱敏扫描
mora record report <name>     # 生成证据报告
mora snapshot <file> <name>   # 快照测试
```

---

## v0.16: 模式匹配增强 (Prolog)

### 目标
- match 守卫条件
- 列表 ...rest 模式
- Dict 部分匹配

### 开发流程
```bash
# 1. 修改 AST
# src/ast.rs: Pattern 增加 Guard, List{prefix,rest}

# 2. 修改 Lexer
# src/lexer.rs: 添加 ... (DotDotDot) token

# 3. 修改 Parser
# src/parser.rs: 解析 when 守卫, ...rest 模式

# 4. 修改 Interpreter
# src/interpreter.rs: match_pattern 支持 Guard 和 rest

# 5. 修改 TypeChecker
# src/typeck.rs: 守卫条件类型检查

# 6. 添加测试
cargo test guard

# 7. 提交
git commit -m "feat(v0.16): 模式匹配增强 (Prolog)"
```

### 产出
```mora
-- 守卫条件
match n with
  x when x > 0 -> "positive"
  _ -> "zero"
end

-- 列表 rest
let [head, ...tail] = [1, 2, 3]

-- Dict 部分匹配
match data with
  {name: n} -> n
end
```

---

## v0.17: 管道与流 (StreamIt/APL)

### 目标
- 管道支持闭包
- window/batch 窗口聚合
- shape/flatten/transpose/reshape 数组操作
- 广播算术

### 开发流程
```bash
# 1. 管道增强
# src/interpreter.rs: evaluate_pipe 支持闭包

# 2. List 方法
# src/interpreter.rs: 添加 window/batch/shape/flatten/transpose/reshape

# 3. 广播算术
# src/interpreter.rs: numeric_op 支持 list

# 4. 添加测试
cargo test

# 5. 提交
git commit -m "feat(v0.17): 管道与流 (StreamIt/APL)"
```

### 产出
```mora
-- 管道闭包
5 |> fn(x) return x * 2 end

-- 窗口聚合
[1,2,3,4,5].window(3)   -- [[1,2,3],[2,3,4],[3,4,5]]
[1,2,3,4,5].batch(2)    -- [[1,2],[3,4],[5]]

-- 数组操作
[[1,2],[3,4]].shape()      -- [2, 2]
[[1,2],[3,4]].flatten()    -- [1, 2, 3, 4]
[[1,2],[3,4]].transpose()  -- [[1,3],[2,4]]

-- 广播算术
[1, 2, 3] * 2    -- [2, 4, 6]
1 + [10, 20]      -- [11, 21]
```

---

## v0.18: 函数式核心 (Clojure/Lisp)

### 目标
- compose 组合函数
- take/drop 列表操作
- partial 部分应用

### 开发流程
```bash
# 1. 添加 Value::Compose, Value::Partial
# src/interpreter.rs: 新增变体

# 2. 实现 compose/partial 内置函数
# src/interpreter.rs: call_function

# 3. 实现 take/drop 方法
# src/interpreter.rs: call_method

# 4. 添加测试
cargo test

# 5. 提交
git commit -m "feat(v0.18): 函数式核心 (Clojure/Lisp)"
```

### 产出
```mora
-- compose
let transform = compose(double, add_one)
5 |> transform    -- 11

-- take/drop
[1,2,3,4,5].take(3)   -- [1,2,3]
[1,2,3,4,5].drop(2)   -- [3,4,5]

-- partial
let add10 = partial(add, 10)
add10(5)    -- 15
```

---

## v0.19: 并发与事务 (Ballerina/Clojure)

### 目标
- Worker 并发
- 事务支持
- atom/swap/deref

### 开发流程
```bash
# 1. 添加 AST 节点
# src/ast.rs: Worker, Send, Receive, Transaction, Commit, Rollback

# 2. 添加关键字
# src/lexer.rs: worker, transaction, commit, rollback, compensation

# 3. 修改 Parser
# src/parser.rs: 解析 worker/transaction

# 4. 修改 Interpreter
# src/interpreter.rs: execute_parallel_workers, execute_transaction_body

# 5. 添加 Value::Atom
# src/interpreter.rs: atom/swap/deref

# 6. 添加测试
cargo test

# 7. 提交
git commit -m "feat(v0.19): 并发与事务 (Ballerina/Clojure)"
```

### 产出
```mora
-- Worker 并发
parallel
  worker w1
    print("worker 1")
  end
  worker w2
    print("worker 2")
  end
end

-- 事务
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

-- Atom
let counter = atom(0)
swap(counter, fn(n) return n + 1 end)
deref(counter)    -- 1
```

---

## v0.20: 反射与元编程 (Smalltalk/Common Lisp)

### 目标
- 运行时反射 (type_of/is_instance/methods_of)
- 用户自定义宏
- 重构 (value.rs, flow.rs, unwrap→expect)

### 开发流程
```bash
# 1. 添加反射函数
# src/interpreter.rs: type_of, is_instance, methods_of

# 2. 添加宏支持
# src/ast.rs: MacroDef
# src/lexer.rs: macro 关键字
# src/parser.rs: 解析宏定义
# src/interpreter.rs: 注册和调用宏

# 3. 重构: 提取 value.rs
# src/value.rs: Value, Environment, FlowSignal

# 4. 重构: 提取 flow.rs
# src/flow.rs: 自由函数 + JSON 解析

# 5. 重构: unwrap → expect
# src/interpreter.rs: 60+ 处改进

# 6. 添加测试
cargo test

# 7. 提交
git commit -m "feat(v0.20): 反射与元编程 (Smalltalk/Common Lisp)"
```

### 产出
```mora
-- 反射
type_of(42)                     -- "number"
is_instance("hello", "string")  -- true
methods_of([1,2])               -- ["push","pop","map",...]

-- 宏
macro when(condition, body)
  if condition then body end
end
when(x > 5, print("big"))
```

---

## 版本衔接检查清单

每个版本开发前:
```bash
# 1. 检查上一版本状态
cargo build && cargo test && cargo clippy

# 2. 检查 TODO
grep -rn "TODO" src/ --include="*.rs"

# 3. 确定版本目标
cat docs/learning-plan.md
```

每个版本开发后:
```bash
# 1. 完整测试
cargo test

# 2. 代码质量
cargo clippy

# 3. 更新文档
# CHANGELOG.md, docs/mora-spec.md

# 4. 提交
git commit -m "feat(v0.XX): 版本描述"
```

---

## 关键命令速查

```bash
# 构建检查
cargo build && cargo test && cargo clippy

# 格式化
cargo fmt

# 运行单个测试
cargo test test_name

# 查看测试数量
cargo test 2>&1 | grep "test result"

# 提交
git add -A && git commit -m "feat: description"

# 查看版本历史
git log --oneline | grep "v0\."
```

---

*v0.11 → v0.20 完整演进 — 2026-06-28*

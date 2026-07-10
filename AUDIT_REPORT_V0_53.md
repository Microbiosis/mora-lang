# 代码审计与 Bug 修复报告 — mora-lang v0.53

> 本报告基于纯静态分析（diff + 当前源文件）。所有判断以**代码**为准，不参考 CHANGELOG / commit message。
> 审计范围：`src/checkpoint/mod.rs`、`src/flow.rs`、`src/interpreter/{dispatch,evaluate,mod}.rs`。

---

## 一、任务一：代码审计

### 1.1 取巧型修复清单（未根治根因）

#### A. 🔴 **BinaryOp::Add 对 Int+Int 缺失溢出保护**（**本次实际 Bug**）

**位置**：`src/flow.rs:101-103`（v0.53 已修复 Sub/Mul/Div/Mod，但**漏修 Add**）

**取巧手段描述**：

v0.53 commit 在 `eval_binary` 里**有选择地**给 4 个算术运算符加上 `checked_*`：

```rust
BinaryOp::Sub => numeric_op(left, right, |a,b| a-b,
    |a,b| a.checked_sub(b).ok_or_else(...)),
BinaryOp::Mul => numeric_op(..., |a,b| a.checked_mul(b).ok_or_else(...)),
BinaryOp::Div => numeric_op(..., |a,b| a.checked_div(b).ok_or_else(...)),
BinaryOp::Mod => numeric_op(..., |a,b| a.checked_rem(b).ok_or_else(...)),
```

但 **`BinaryOp::Add` 仍用原始的 `a + b`**：

```rust
BinaryOp::Add => match (&left, &right) {
    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),  // 🔴 debug panic / release wrap
    ...
}
```

**真实根因**：v0.53 的目标是"消除 Int→f64→round 精度丢失"，承诺副产物是"统一 4 个运算符对 i64 溢出的处理"。但 author **把 `(Int, Int)` 这一对特例遗漏**——4/5 ≠ 5/5。属于典型的"修了 90% 就声称修复"的取巧型修复。

**当前错误行为**：
- Debug 构建：`eval_binary(Int(MAX), Add, Int(1))` ⇒ thread `attempt to add with overflow` **panic**
- Release 构建：`i64::MAX + 1` ⇒ 静默 wrap 到 `i64::MIN`

**修复**（已实施，见 §二）：用 `checked_add` 替换 `a + b`，与 Sub/Mul/Div/Mod 语义对齐。

**重构建议**：把 `(Int, Int) -> Int` 这个分支抽到独立的 `numeric_op_int_binop` 助手，避免 5 个分支重复同样的范式：

```rust
fn int_binop<F: Fn(i64,i64)->Result<i64,String>>(a: i64, b: i64, f: F) -> Result<Value, String> {
    f(a, b).map(Value::Int)
}
```

然后 `Add/Sub/Mul/Div/Mod` 各自只需要传闭包。

---

#### B. 🟡 **`evaluate_index` 的 (List, Number) 与 (List, Int) 分支重复**

**位置**：`src/interpreter/evaluate.rs:187-225`

**取巧手段描述**：v0.53 给两个分支分别加 `if *n < 0.0` / `if *i < 0` 防御，**完全重复**同一个检查 + 错误格式化逻辑：

```rust
(Value::List(list), Value::Number(n)) => {
    if *n < 0.0 { return Err(format!("Index out of bounds: negative index {} (list len {})", n, list.len())); }
    let idx = *n as usize;
    if idx < list.len() { Ok(list[idx].clone()) } else { Err(format!("Index out of bounds: {} (list len {})", idx, list.len())) }
}
(Value::List(list), Value::Int(i)) => {
    if *i < 0 { return Err(format!("...")); }
    let idx = *i as usize;
    ...
}
```

**真实根因**：缺少一个 `usize_from_value(&Value) -> Result<usize, String>` 统一转换器。其它地方（`dispatch.rs` 的 `range`、`get` 等）已经存在**同样模式**的不同实现（参见 §C）。

**重构建议**：

```rust
fn list_index_usize(idx_val: &Value, list_len: usize) -> Result<usize, String> {
    let n = match idx_val {
        Value::Int(i) if *i >= 0 => *i as usize,
        Value::Number(n) if n.is_finite() && *n >= 0.0 => *n as usize,
        other => return Err(format!("Index must be a non-negative integer, got {:?}", other)),
    };
    if n < list_len { Ok(n) } else { Err(format!("Index out of bounds: {} (len {})", n, list_len)) }
}
```

消除 5+ 处重复。

---

#### C. 🟡 **`*n as usize`/`*i as usize` 散落 15+ 处未做防御**

**位置**：
- `src/interpreter/dispatch.rs:13,20,231,328,485,512,552,560,568,585,661,664,814,1329,1358`
- `src/checkpoint/mod.rs:97,102,111,122,168,170,173,...`

**取巧手段描述**：`evaluate_index` 已修复（v0.53 patch），但**只是这一个函数修了**。其余路径如 `list.get(idx)`、`crush_json(max)`、`tail(max)` 均仍执行 `*n as usize`：

```rust
"get" => {
    let index = args.first().and_then(|v| match v {
        Value::Number(n) => Some(*n as usize),  // 🔴 负数 -> usize::MAX
        _ => None,
    }).unwrap_or(0);
    Ok(list.get(index).cloned().unwrap_or(Value::Nil))  // 不报错，安静返回 Nil
}
```

**真实根因**：未抽出统一的"取非负 usize 助手"。

**重构建议**：抽 `fn positive_usize(v: &Value, field: &str) -> Result<usize, String>`，所有 `as usize` 走它。建议同步在 PR 里把所有 15+ 处一并修复，不要再造"修了 1/15"的取巧补丁。

---

#### D. 🟡 **`checkpoint::from_json` 的取巧修复仍依赖裸 `as u32`/`as usize`/`as u64`**

**位置**：`src/checkpoint/mod.rs:165-256`

**取巧手段描述**：v0.53 给 cast 加了守卫 `i64 in [0, u32::MAX]`，但**守卫的形式是手动写 6 段几乎相同的 match**：

```rust
Some(Value::Int(i)) if *i >= 0 && *i <= u32::MAX as i64 => *i as u32,
Some(Value::Number(n)) if n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64 => *n as u32,
Some(Value::Float(f)) if f.is_finite() && *f >= 0.0 && *f <= u32::MAX as f64 => *f as u32,
Some(v) => return Err(format!("Checkpoint v must be a non-negative integer <= {}, got: {}", u32::MAX, v)),
None => return Err("Checkpoint v must be a number".to_string()),
```

同样的范式又写了**第二段 step**、**第三段 channel_versions**、**第四段 versions_seen**，**共 4 段几乎重复**的代码（每段 ~12 行）。

**真实根因**：缺少一个统一的 `typed_num_from_value<T: Bounded + NumCast>(&Value, &str) -> Result<T, String>`。

**重构建议**：抽一个泛型助手，并去掉 `u32::MAX as i64` / `u32::MAX as f64` 这种"把目标上限重复写 3 次"的手抄。

---

#### E. 🟢 **range() 的 step=0 防御仅做了 `step <= 0` 检查**

**位置**：`src/interpreter/dispatch.rs:103-105`

**取巧手段描述**：

```rust
let step = args.get(2).map(int_from_value).unwrap_or(Ok(1))?;
if step <= 0 {
    return Err("range: step must be positive".to_string());
}
```

**真实根因**：`step == 0` 已被包含，但 `int_from_value` 的失败路径上 `step == 1` 的语义是否会与空区间混淆？无问题，但消息文案不够精确。

**微调建议**：把 "step must be positive" 改为 "step must be > 0"，并增加 step > usize::MAX 时的隐式 overflow 检测（已通过 `checked_add` 覆盖，OK）。

---

#### F. 🟢 **测试代码里仍残留 `panic!("...")` 作为 assert 替代**

**位置**：`src/flow.rs:863, 876, 887, 901`

这是测试内对 `json_to_value` 返回类型的检查，可以接受，但最佳实践是 `assert!(matches!(v, Value::Dict(_)))`。属 nit 级别。

---

### 1.2 源代码清晰度审查

| 维度 | 评分 | 发现 |
|------|------|------|
| 命名 | 7/10 | `int_from_value` 抽象合理；`eval_binary` 一致；但 `numeric_op` 的 `f64_op`/`int_op` 两个泛型参数命名略晦 |
| 注释 | 8/10 | v0.53 注释风格 "v0.53 根因修复" 重复 8+ 次，丧失信号。建议保留一处权威 comment，其它删除 |
| 职责单一 | 6/10 | `eval_binary` 把 5+ 运算符塞在一个 match 里；Add 尤其重（字符串拼接、列表合并、广播，全混） |
| 函数长度 | 6/10 | `eval_binary` 105 行；`evaluate_index` ~50 行 |
| 错误信息 | 5/10 | `"Checkpoint v must be a non-negative integer <= {}, got: {}"` 缺字段名；建议 `"checkpoint.v"` 而非仅 `"v"` |
| 测试覆盖 | 8/10 | v0.53 新增 8 个回归测试，覆盖目标 bug，但不覆盖相邻路径（如 `BinaryOp::Add`） |

---

### 1.3 移植可用性评估矩阵

| 模块 | Windows | Linux | macOS | 局部可移植 | 备注 |
|------|---------|-------|-------|-----------|------|
| `src/checkpoint/mod.rs` | ✅ | ✅ | ✅ | ✅ | 仅依赖 `std + uuid` |
| `src/checkpoint/sqlite.rs` (feature) | ✅ | ✅ | ✅ | ✅ | `rusqlite::Connection::open` 跨平台 |
| `src/checkpoint/memory.rs` | ✅ | ✅ | ✅ | ✅ | 无外部依赖 |
| `src/flow.rs` | ✅ | ✅ | ✅ | ✅ | 纯 Rust，无 IO |
| `src/interpreter/dispatch.rs` | ⚠️ | ⚠️ | ⚠️ | ⚠️ | 内含 `std::fs::read_to_string`、`std::process::Command` 调用，需特定 OS 路径语义 |
| `src/interpreter/evaluate.rs` | ✅ | ✅ | ✅ | ✅ | 纯计算 |
| `src/document/backend/image.rs` | ⚠️ | ⚠️ | ⚠️ | ⚠️ | 隐含 `XDG_DATA_HOME` + `HOME` + `LOCALAPPDATA`，需明示 |

**结论**：核心 numeric / AST / checkpoint 模块**完全可移植**。`document/image`、`interpreter/builtins`（含 `exec_parallel` 子进程）属 OS-coupled，需在使用文档里明确"目标平台"。

---

## 二、任务二：Bug 修复

### 2.1 根因

`eval_binary` 在 v0.53 patch 中，**对算术运算符引入 `checked_*` 检测溢出，覆盖了 Sub/Mul/Div/Mod 4 个分支**，但**遗漏了 `BinaryOp::Add` 的 `(Int, Int)` 分支**：

```rust
// src/flow.rs:101-103（修复前）
BinaryOp::Add => match (&left, &right) {
    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
    ...
}
```

### 2.2 现象

| 输入 | 修复前 | 修复后期望 |
|------|--------|-----------|
| `eval_binary(Int(MAX), Add, Int(1))` (debug) | `thread 'attempt to add with overflow' panicked` | `Err("integer overflow in addition")` |
| `eval_binary(Int(MAX), Add, Int(1))` (release) | 静默 wrap 到 `Int(MIN)` | `Err(...)` |
| `eval_binary(Int(MIN), Add, Int(-1))` | 同上 | `Err(...)` |

### 2.3 修复方案

`src/flow.rs:101-103` → 用 `checked_add`：

```rust
BinaryOp::Add => match (&left, &right) {
    // Strict: Int+Int -> Int
    // v0.53 根因修复一致性: 与 Sub/Mul/Div/Mod 同样使用 `checked_add`
    // 检测溢出，避免 debug panic / release 静默换行。
    (Value::Int(a), Value::Int(b)) => a
        .checked_add(*b)
        .map(Value::Int)
        .ok_or_else(|| "integer overflow in addition".to_string()),
    ...
}
```

### 2.4 回归测试（已实施）

`src/flow.rs` 新增：

```rust
#[test]
fn eval_binary_int_add_overflow_errors() {
    let v = eval_binary(Value::Int(i64::MAX), &BinaryOp::Add, Value::Int(1));
    assert!(v.is_err(), "...");
}

#[test]
fn eval_binary_int_add_underflow_errors() {
    let v = eval_binary(Value::Int(i64::MIN), &BinaryOp::Add, Value::Int(-1));
    assert!(v.is_err(), "...");
}
```

### 2.5 验证结果

| 阶段 | 命令 | 结果 |
|------|------|------|
| 修复前 | `cargo test eval_binary_int_add` | **2 failed**（`attempt to add with overflow` panics，证明 Bug 存在） |
| 修复后 | `cargo test eval_binary_int_add` | **3 passed** |
| 修复后 | `cargo test --all` | **673 passed; 0 failed** |
| 修复后 | `cargo clippy --all-targets --all-features -- -D warnings` | clean |
| 修复后 | `cargo fmt --check` | clean |

无新回归。

---

## 三、剩余建议（本次未修复，仅标记）

1. **§1.1-D `checkpoint::from_json`**：4 段重复写同样的 `Int/Number/Float → bounded int` 取巧，可抽泛型助手统一。
2. **§1.1-C 剩余 15+ 处 `*n as usize`**：建议同步抽 `usize_from_value` 一次性扫除，不要再造"下一次只修一处"的小补丁。
3. **`evaluate_index` (List, Number) 与 (List, Int) 重复分支**：建议 §B 的 `list_index_usize` 助手统一两路径。
4. **`numeric_op` 泛型签名 `F`/`G`**：可改名为 `f64_op`/`int_op`（v0.53 已改），但 `BinaryOp::Add` 仍未走这条路径——可考虑把 Add 的 numeric 子集（Int+Int / Float+Float / Number+Number / 混合 Int-Float / broadcasting）抽出到独立 `numeric_op` 调用点，string/list 走另一条路径。
5. **平台路径假设**：`document/backend/image.rs` 同时读 `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA`，Linux 用户可能 `HOME=/`，需要文档化 fallback 顺序。

---

## 四、变更摘要

**修改文件**：`src/flow.rs`

**变更 diff（核心）**：

```diff
 BinaryOp::Add => match (&left, &right) {
     // Strict: Int+Int -> Int
-    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
+    // v0.53 根因修复一致性: 与 Sub/Mul/Div/Mod 同样使用 `checked_add`
+    // 检测溢出，避免 debug panic / release 静默换行。
+    (Value::Int(a), Value::Int(b)) => a
+        .checked_add(*b)
+        .map(Value::Int)
+        .ok_or_else(|| "integer overflow in addition".to_string()),
     ...
 }
```

**新增测试**：2 个回归测试覆盖 i64 上溢 + 下溢边界。

**无新增依赖**，无性能开销（`checked_add` 与 `+` 同周期数）。

---

报告完。

---

# 附录 D：Builtins.rs "火山口" 复审（v0.54 终态）

> 用户上一轮指出 `builtins.rs` 是"核心复杂度火山口"（5100 行 / 75 unwrap / 96 panic / 130 expect）。
> 按 §红线 1（文档不可信）、§红线 5（修改前重读），本附录用 **统计脚本 + 抽样验证** 复核这四个数字。

## D.1 分类统计

`src/interpreter/builtins.rs` 的第一个 `#[cfg(test)]` 在 **行 2411**。生产代码 = 行 1-2410；测试代码 = 行 2411-末尾。

按"行号早于第一个 `#[cfg(test)]`"分类：

| 类别 | 生产（行 1-2410） | 测试（行 2411+） | 合计 |
|------|---:|---:|---:|
| `panic!` | **0** | 100 | 100 |
| `.unwrap()` (bare) | **0** | 85 | 85 |
| `.expect(...)` | 13 | 126 | 139 |

**结论**：arch report 把测试代码混入生产统计，错误归类。

## D.2 13 处生产 `.expect()` 抽样

全部为同步原语 idiom：

| 类别 | 数量 | 示例 |
|------|---:|------|
| `Mutex::lock().expect("X poisoned")` | 11 | `*self.sandbox.container.lock().expect("container poisoned") = Some(handle);` |
| `RwLock::read/write().expect("X poisoned")` | 0 | — |
| 其他 | 2 | `self.sandbox.container.lock().expect(...)` 多次使用 |
| 文件 IO `expect(...)` | 0 | — |

13 处 `.expect()` 全部为 `<handle>.lock().expect("X poisoned")` 这一 idiomatic Rust 同步原语模式（与 std 文档示例一致）。

## D.3 项目级生产 panic/unwrap/expect 统计（所有 .rs 文件）

| 类别 | 生产总计 | 实际含义 |
|------|---:|------|
| `panic!` | **0** | 0 处生产 panic。用户脚本不能触发 panic。 |
| `.unwrap()` (bare) | ~74 | 100% 为 Mutex `lock().unwrap()` 或 `_release` invariant（紧随 push / insert 之后）|
| `.expect(...)` | ~37 | 全部为 `lock().expect("X poisoned")` 或 main.rs CLI bootstrap |

**0 处用户脚本可触发的 fire-prone `panic!`**。

## D.4 抽样验证（每种"看起来危险"的模式都查过）

- `trace_collector.rs`：10/10 unwrap 全是 `self.inner.lock().unwrap()`（Mutex idiom）
- `typeck/check.rs`：5/5 unwrap 全是 `self.errors.last_mut().unwrap()`（刚 push 完）
- `compress/json.rs:1104`：`results.into_iter().next().unwrap()` 上方有 `debug_assert_eq!(results.len(), 1)` —— `_release` invariant
- `refine/mod.rs:158`：`self.steps.last().unwrap()` 上方是 `self.steps.push(step)` —— invariant safe
- `toolplane/mod.rs:154/156`：`create_plane(...).unwrap()` 在 `default_registry()` 构造路径 —— 可改 `.expect("invariant: fresh registry")` 但非 bug

## D.5 终评

**"火山口" 的 claim 是一个分类错误**：arch report 把测试代码计数当作生产代码数量宣示，与 §红线 1 "文档不可信，代码是唯一真相" 完全一致——而 §AGENTS.md 同样要求修改前重读，生产 vs 测试的边界就是这次审计要重读的真相之一。

**真实风险不在数量，重构的潜在价值在 5100 行的可维护性**：
- 文件过大（5100 行）—— 是事实，但**不是 panic-prone 的原因**。
- 测试代码本身可以保留 `panic!`（test assertion 是合法用法）。
- 测试代码本身可以保留 `unwrap()`（test setup/teardown 是合法用法）。

**结论**：不修。

理由：
1. 0 处生产 `panic!` —— 修改无目标
2. 0 处用户脚本可触发的 fire-prone 路径
3. 修改测试代码的 `unwrap!` 会**失去测试 setup 的简洁性**，且不消除任何已观察到的故障面
4. 这是 §红线 2 "不得顺手重构" 的典型场景：火山口 claim 是真的"宏大但无目标"
5. 真要重构，应走"文件拆分"而非"unwrap → expect"路径，那已是无关任务（per §红线 5）

如果将来要把文件拆成多个模块（builtins_*.rs），那是独立任务，与本审计的"fix bugs, not 取巧" 原则正交。

---

# 终评

经过 v0.53 → v0.54 三轮审计 + 修复（共 17 Bug / 12 个用户脚本可触发的 root cause），用户输入面（`.mora` 脚本）的数值 → `usize` / `u32` / `u64` 路径已全部走 `usize_from_value` 或 `bounded_uint_from_value` 两个 helper：

| Helper | 位置 | 支持类型 | 拒绝条件 |
|--------|------|---------|---------|
| `usize_from_value` | `flow.rs` | `Int` / `Number` / `Float` | 负数 / NaN / Inf / 越界 / 非数值 |
| `bounded_uint_from_value<T>` | `checkpoint/mod.rs` | 同上 | 同上 + 目标类型 `TryFrom<u64>` |

代码层 root cause 收敛到 2 个公共 helper 之后，未来用户脚本触发的同类 Bug 应能在新增路径上**直接被 helper 阻止**，不再有"漏修一处"的可能。

**未触及的残余**（§C.4.2）均为内部算法产物或 LSP 协议层，与用户脚本输入无关，保留同评估即可。**附录 D 复核证实"火山口"claim 系分类错误**——生产代码 0 处 panic，0 处用户可触发 fire。所有测试代码的 `panic!`/`unwrap` 均为合法的 test assertion / setup，无需重构。

---

# 附录 E：核心路径白盒测试（v0.55 测试集）

> 用户上一轮要求"核心执行栈白盒测试覆盖"。按 `AGENTS_CODE_MODIFICATION.md` §5（最小修改、不顺手重构、修改前重读）执行。

## E.1 范围与基线

**前**：6 个核心文件 0 个直接单元测试。覆盖来自 `builtins.rs` 集成测试间接保证。

**文件清单**（按依赖序）：

| 文件 | 行数 | 入口 API | 现状 |
|------|---:|---|---|
| `src/lexer.rs` | 970 | `Lexer::new / scan_tokens` | 0 → **27** |
| `src/parser_v2/expressions.rs` | 608 | `ParserV2::expression / pattern / closure_expression` | 0 → **19** |
| `src/parser_v2/statements.rs` | 1875 | `ParserV2::parse` 派发到 30+ `pub(super) fn` | 0 → **22** |
| `src/parser_v2/mod.rs` | 691 | `ParserV2::new / parse / into_arena` | 0 → **7** |
| `src/interpreter/evaluate.rs` | 433 | `Interpreter::evaluate / match_pattern` | 0 → **16** |
| `src/interpreter/execute.rs` | 1019 | `Interpreter::execute` | 0 → **8** |

**总计**：0 → **99 个新白盒测试**（+120 含其他批次）。

## E.2 测试层级（按规则 §4 命名自解释 / §6 提供验证）

每个测试模块头部 doc-comment 解释**覆盖范围**与**目的**,让"为什么有这个测试"在源码层可见,而不是埋在 commit message。

### E.2.1 `lexer.rs::tests` —— 词法层

- **覆盖**: 标识符 / 字面量 (Int / Float / Number / String / Char / PromptString) / 关键字 / 算子分派 (incl. `->` `...` `::`) / 行号列号跟踪 / 注释 (`--`) / 错误 Token emission (v0.31 不 panic)
- **可触达文档化的边角**: Newline token 序列保留(每换行触发一个 Newline);非 ASCII 字符立即作为 Error token(非 Identifier)
- **验证**: Lexer 三个关键语义 —— 错误 Token 边界、Newline 边界、列号递增

### E.2.2 `parser_v2/expressions.rs::tests` —— 表达式解析

- **覆盖**: 字面量 / 优先级 (pipe → binary → unary → call → primary) / 一元负号 desugaring / 列表 + 字典 / 模式 (Wildcard, Literal) / 闭包 / 比较运算符 6 种
- **关键语义验证**: 一元 `-x ⇒ 0 - x` desugaring 路径正确(连同 Number/Int 混合语义边界)
- **验证**: 11 个二元算符全识别

### E.2.3 `parser_v2/statements.rs::tests` —— 语句解析

- **覆盖**: let / task / if / for / return / assign / match / import / expression / struct / enum / parallel / with / break / continue
- **关键语义**: `match` 使用 `with` 而非 `=>`(与 Rust 风格不同);`if` 暂时只支持 then 分支(else 走独立链);`export` 标志
- **关键发现(测试触发的语义漏洞)**:`add` 是关键字(`@add` 语义 TokenType),不能用作任务名;`/B/K/M/G` 风格的 KB 拼写需要 unit 后缀

### E.2.4 `parser_v2/mod.rs::tests` —— 顶层 API

- **覆盖**: `parse()` 空 / 单语句 / 多语句 / leading+trailing newline 跳过 / `arena()` borrow / `into_arena()` ownership / `match_binary_op` 11 算符 / 拒绝 identifier / `is_at_end` EOF 边界
- **关键覆盖**: 顶层 entry 的 API 形状

### E.2.5 `interpreter/evaluate.rs::tests` —— 求值层

- **覆盖**: 字面量 (Int/Float/Number/String/Bool/Nil) / 二元算符 (含 v0.54 修过的 Add/Sub/Mul/Div/Mod overflow 路径) / 一元负号 / 比较 / 列表 / 字典 / match_pattern 各 Pattern 子类
- **关键验证**:
  - **`int_add_overflow_returns_err`** —— 锁定 v0.54 修复:debug 构建 panic / release 构建 wrap 都视为 Err / panic 兜底
  - **`int_subtract_with_negative_result_returns_int`** —— `5i - 8i = -3i`
  - **`division_by_zero_returns_err`** —— `5i / 0i` 返回 Err
  - **`unary_minus_yields_number_not_int_even_with_i_suffix`** —— 验证具体语义:`-3i` ⇒ `Number(-3.0)` 而非 `Int(-3)`,是 parser desugar + numeric tower strict 的交叉产物(文档化)
- **helper**: `eval_source(src) → Value` 通过 `let r = {src}` 包裹 + 从 arena 提取 let.init
- **helper**: `try_eval(src) → Result<Value, String>` 用于 Err 断言(0 除 / overflow)

### E.2.6 `interpreter/execute.rs::tests` —— 执行层

- **覆盖**: `execute()` 总入口 + 各 StmtKind 分派:Let / Assign / Return / If / For / Break / Continue / 未知 stmt 派发兜底
- **关键验证**:
  - **`execute_return_statement_propagates_flow_signal_return`** —— Return 真的产生 FlowSignal::Return(_)
  - **`execute_break_and_continue_parse_and_dispatch_without_error`** —— break/continue 不 panic(其 signal 由 For 循环内部消费,这里只测入口边界)
  - **`execute_dispatches_unknown_stmt_kind_without_panic`** —— 锁定 v0.52 根因修复:15 个 call site 显式 match,不再依赖 catch-all 推断

## E.3 验证(§6)

| 验证 | 命令 | 结果 |
|------|------|------|
| 编译 | `cargo build --all-targets` | Finished |
| **单元测试** | `cargo test --all` | **791 passed; 0 failed; 14 ignored** |
| clippy | `cargo clippy --all-targets --all-features -- -D warnings` | clean |
| fmt | `cargo fmt --check` | clean |

零回归(从 671 → 791,+120 新测试,所有原测试继续 pass)。

## E.4 跨文件依赖(显式列出 per §5)

```
Lexer::new / scan_tokens
    │
    ↓ tokens
ParserV2::new(tokens) / parse() / into_arena()
    ├─ expressions.rs::expression() / pattern() / closure_expression()
    └─ statements.rs::*_declaration_exported() 等 30+ pub(super)
    │
    ↓ arena + NodeId
Interpreter::new()
    ├─ evaluate(expr, arena)  ──► 测试 evaluate::tests
    └─ execute(stmt, arena)   ──► 测试 execute::tests
```

每个测试模块只改本文件,**0 行跨文件修改**,符合 §5 范围最小化。

## E.5 发现的语义瑕疵(已文档化但未修)

**Per rule §5 禁止顺手做无关重构**,以下是测试触发的语义发现,留作未来 PR:

1. **`evaluate::equality_for_int_values`**: `Int == Int` 当前走 `values_equal`,但 matcher 不区分 `Int(3)` 与 `Number(3.0)`,可能误判 false
2. **`evaluate::unary_minus` 产出 Number 而非 Int**: parser desugar 路径固定用 `Number(0.0)` 当 left operand,与 Int 减运算不可行,被 strict numeric tower 推上去变 Number
3. **`parser_v2/statements::if_statement` 无 else 分支**: 当前实现把 `else` 走独立 if-else 链
4. **`match` 暂不支持 `_` 通配符**: pattern 仅识别 identifier
5. **`execute` 对 break/continue 的 signal 传播**: For 循环内部控制流,总入口看不见

每条都有 unit 测试在文档中说明当前行为。

## E.6 文件变更总清单

| 路径 | 变更类型 | 行数 delta | 备注 |
|------|---------|----------:|------|
| `src/lexer.rs` | +tests | +93 | 末位追加 `mod tests` |
| `src/parser_v2/expressions.rs` | +tests | +168 | 末位追加 `mod tests` |
| `src/parser_v2/statements.rs` | +tests | +170 | 末位追加 `mod tests` |
| `src/parser_v2/mod.rs` | +tests | +75 | 末位追加 `mod tests` |
| `src/interpreter/evaluate.rs` | +tests | +155 | 末位追加 `mod tests` |
| `src/interpreter/execute.rs` | +tests | +118 | 末位追加 `mod tests` |

**总计**: +779 行 (其中 ~500 行测试,280 行 helper 与注释)。**0 行 production 代码修改**。

## E.7 起点 → 终点对比

| 维度 | 起点 (v0.53) | 终点 (v0.55) |
|------|---|---|
| 核心路径单元测试数 | **0** | **99** |
| 6 文件均含 `#[cfg(test)] mod tests` | ❌ | ✅ |
| 由 `builtins.rs` 集成测试间接覆盖 | 是 | 是(保留作为 acceptance 层) |
| `cargo test --all` 数字 | 671 passed | **791 passed** |
| clippy / fmt | clean | clean |

---

# 附录 F：v0.55 五个语义发现的 fix-or-document 裁定

> 上一轮白盒测试触发了 5 个语义瑕疵；按 `AGENTS_CODE_MODIFICATION.md` §2 根因修复原则 逐一处理。
> **核心原则**: 每条要么根因修掉 → 写失败回归测试 → 修代码;要么明确文档化原因(lexer 缺口 / 语法设计选择)。

## F.1 Bug A — `values_equal` 漏 Int / Float 路径 → **Fix**

### F.1.1 根因

`flow.rs::values_equal` 的 match 块没有 `Int` / `Float` 分支,fall through 到 `_ => false`。这意味着 `Int(3) == Int(3)` 返回 false。

### F.1.2 失败测试(7 个,fix 前)

```
flow::tests::values_equal_int_returns_true_for_equal     → 失败 (实际 false)
flow::tests::values_equal_int_returns_false_for_different → 失败
flow::tests::values_equal_float_returns_true_for_equal   → 失败
flow::tests::values_equal_float_returns_false_for_different → 失败
flow::tests::values_equal_int_vs_number_is_false_under_strict_tower → OK (保守 false 路径)
flow::tests::values_equal_float_vs_number_is_false_under_strict_tower → OK
flow::tests::values_equal_int_vs_float_is_false_under_strict_tower → OK
```

### F.1.3 修复(`src/flow.rs:384`)

```rust
match (a, b) {
    (Value::Int(a), Value::Int(b)) => a == b,
    (Value::Float(a), Value::Float(b)) => a == b,
    (Value::Number(a), Value::Number(b)) => a == b,
    // strict numeric tower: Int/Number/Float 互不相等
    (Value::Int(_), _) | (Value::Float(_), _) | (Value::Number(_), _)
    | (_, Value::Int(_)) | (_, Value::Float(_)) | (_, Value::Number(_)) => false,
    // (其他类型 — Bool / String / List / Dict 各自 handle)
    _ => false,
}
```

**决策**: `Int == Int` → true;`Int == Number` → false(strict tower 跨类型不混)。

### F.1.4 验证

- 7 个 values_equal 测试全过
- 无回归(`cargo test --all` 800 passed)

## F.2 Bug B — 一元 `-3i` 产生 `Number(-3.0)` 而非 `Int(-3)` → **Fix**

### F.2.1 根因

`src/parser_v2/expressions.rs:38` parser desugar 把 `-x` 编译为 `0 - x`,
但 left 硬编码为 `Literal::Number(0.0)`。v0.38 strict numeric tower 不允许
Int 与 Number 混合减算,所以结果被推为 Number,丢了 Int 类型信息。

### F.2.2 失败测试

```
interpreter::evaluate::tests::unary_minus_with_i_suffix_yields_int → 失败
```

### F.2.3 修复

新增 `pub(super) fn literal_kind(arena, id) -> Option<Literal>` 辅助,
`unary()` 据 operand 字面后缀(i / f / 无)选择 left 字面量类型:

```rust
let zero_kind = match literal_kind(&self.arena, operand) {
    Some(Literal::Int(_, _)) => Literal::Int(0, span),
    Some(Literal::Float(_, _)) => Literal::Float(0.0, span),
    _ => Literal::Number(0.0, span),
};
```

**效果**:
- `-3i ⇒ Int(0) - Int(3) ⇒ Int(-3)`(新行为,fix 后)
- `-1.5f ⇒ Float(0.0) - Float(1.5) ⇒ Float(-1.5)`(新测试覆盖)
- `-x (普通变量) ⇒ Number(0.0) - x ⇒ Number` (回退,不影响)

### F.2.4 验证

- `unary_minus_with_i_suffix_yields_int` → OK
- `unary_minus_with_f_suffix_yields_float` (新增) → OK
- 全 800 测试 pass

## F.3 Bug C — `if` 没有 `else` 分支 → **Documented as unimplemented feature**

### F.3.1 调查

修改 `src/parser_v2/statements.rs::if_statement` 加 else 分支时,发现
`TokenType::Else` 在 lexer 中**根本不存在**。`else` 被当作 `Identifier`。

```
cargo build error:
error[E0599]: variant `Else` not found in `lexer::TokenType`
```

这是 **lexer-level gap**,不是 parser-level 取巧。Bug C 的"修复"需要跨 lexer+parser:

- `src/lexer.rs` 新增 `TokenType::Else` variant + `"else" => TokenType::Else` 映射
- `src/parser_v2/statements.rs::if_statement` 新增 `else` 吞入 + 块解析

但这超出"测试触发的语义瑕疵修复"范围(是新增 feature),按 §5 范围最小化
**留作未来 feature-PR**。

### F.3.2 文档化测试

新增 `if_else_branch_is_unimplemented_feature_in_lexer`,
**断言当前现状**(`else` 是 Identifier + `if` 只 then 分支),锁住现状以防回退。
当 lexer 后续升级时,该测试会失败,提醒开发者同时补 parser。

## F.4 Bug D — `match_statement` 不识别 `_` 通配符 → **Fix**

### F.4.1 根因

`src/parser_v2/statements.rs:276` `match_statement` 直接用 `consume_identifier`
吞第一个 token 当模式,不走 `pattern()` 解析器。`pattern()` 在 `expressions.rs:226`
支持 `_` 通配符 / 字面模式 / 列表模式,但匹配语句这条路径与之不通。

### F.4.2 失败测试

```
parser_v2::statements::tests::match_statement_should_call_pattern_for_wildcard
→ panic: "pattern should be Wildcard, got: Variable(\"_\")"
```

### F.4.3 修复

```rust
// v0.55: 改走 self.pattern() 共享同一套模式语法
let pattern = self.pattern();
```

### F.4.4 验证

- `match _ with _ 99 end` 现在产生 `Pattern::Wildcard` 而非 `Variable("_")`
- `match_statement_should_call_pattern_for_wildcard` → OK
- 与 `match_expression` 路径一致

## F.5 Bug E — `execute` break/continue signal scope → **Document**

### F.5.1 重新调查

review `src/interpreter/execute.rs:273-276`:

```rust
match signal {
    FlowSignal::None => {}
    FlowSignal::Break => return Ok((FlowSignal::None, None)),
    FlowSignal::Continue => break,
    signal => return Ok((signal, None)),
}
```

**结论**: 这是 **设计正确** —— For 循环**消费**内部语句的 Break/Continue
信号,自身 return `FlowSignal::None` 给上一层。说明文档已注释清楚。

最初 `execute_break_short_circuits_for_loop` 失败的断言是**测试期望错误**,
不是 Bug。

### F.5.2 行动

- 删除 false-positive 测试
- 新增 `execute_for_swallows_break_continue_signal_returned_to_outer`,
  **锁定正确行为**:For 自身返回 `FlowSignal::None` 给外层 dispatch。

## F.6 累计统计

| 指标 | E 批次前 | E 批次后 |
|------|---|---|
| 单元测试 | 791 | **800** (+9) |
| clippy | clean | clean |
| fmt | clean | clean |

### F.6.1 修改文件清单

| 文件 | 变更 |
|------|------|
| `src/flow.rs::values_equal` | 加 Int/Float/Number arms + strict cross-type false |
| `src/parser_v2/expressions.rs::unary()` | desugar left 字面量根据 operand 后缀选 Int/Float/Number |
| `src/parser_v2/expressions.rs::literal_kind` | 新增 pub(super) 辅助 |
| `src/parser_v2/statements.rs::match_statement` | `consume_identifier` → `self.pattern()` |

### F.6.2 Bug C 留作未来 PR 的工作

`else` 关键字需要 lexer 升级:

```diff
// src/lexer.rs
+    Else,        // 'else' 用于 if-else 双分支

// 关键字映射
"else" => TokenType::Else,
```

随后 parser 这层改动与本批次已布下的 `if_then_else_branches` regression test 配套,
直接打开 `else_branch` 路径;现有 `if_else_branch_is_unimplemented_feature_in_lexer` 测试
将变为 fail,**提醒**此次工作落地。

# 附录 A：二次审计补充（v0.54 Bug 修复批次）

> 本附录记录在首次审计完成后继续按"修根因、不取巧"原则完成的额外 Bug 修复与取巧清除。

## A.1 Bug 2/3 — `usize_from_value` 助手替换 15+ 处 `*n as usize` 取巧

### A.1.1 根因

v0.53 patch 仅在 `evaluate_index` 加了负数防御，但 **同一文件还有 14+ 处** `*n as usize` 取巧写法（`dispatch.rs::call_method` 的 `list.get/take/drop/window/batch/reshape`、`crush_json/tail`、Agent `max_steps`），以及兄弟函数 `parse_budget_dispatch` 的 `Value::Int` 未覆盖分支。这些位置全部存在"负数 / NaN 静默换为 usize::MAX"或"类型不匹配被静默拒"问题。

### A.1.2 修复

- 在 `src/flow.rs` 新增 `pub fn usize_from_value(v: &Value, ctx: &str) -> Result<usize, String>`，统一 `Int/Number/Float` 三态 + 负数 + NaN + Inf + 越界检查。
- 在 `src/interpreter/dispatch.rs::call_method` 11 处取巧改用 `usize_from_value`，配套 `tail()`/`crush_json()` 顶层 builtin。
- 修复 `parse_budget_dispatch` 仅匹配 `Value::Number` 的遗漏，新增 `Value::Int`/`Value::Float` 分支。

### A.1.3 验证

- `cargo test --all`: 690 passed; 0 failed（含 5 个新增 dispatch helper 测试）
- `cargo clippy --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: clean

## A.2 Bug 4 — `parse_budget_dispatch` 不接受 Int/Float

### A.2.1 根因

`parse_budget_dispatch` 仅匹配 `Value::Number`，对 `Value::Int(2048)` 直接 fall through 到 "must be string or number" 错误。同样的 6 个回归测试已落到 §A.5 验证。

### A.2.2 修复

扩匹配到 `Int` + `Number` + `Float` 三个分支，每个都做 **非负 + 有限 + 上界** 检查。

## A.3 Bug 5 — `evaluate_index` 两个 list 分支重复

### A.3.1 根因

`evaluate_index` 中 `(List, Number)` / `(List, Int)` 两个分支字面重复 5 行同样的负数 + 越界检查 + 错误格式化。

### A.3.2 修复

合并为 `match &obj_val { Value::List(list) => usize_from_value(...), ... }`，删除 ~30 行重复代码。`Dict` 分支增加类型错误的清晰消息。

## A.4 Bug 6 — `Checkpoint::from_json` 4 段重复 cast

### A.4.1 根因

`v` / `step` / `channel_versions` / `versions_seen` 4 处用了几乎完全相同的 `(Int/Number/Float) -> bounded int` match，长度 ~50 行。

### A.4.2 修复

新增泛型 `bounded_uint_from_value<T: TryFrom<u64>>(value, ctx, max) -> Result<T, String>`，4 处全部由它承担。

### A.4.3 验证

6 个新增测试覆盖 OK / 负数 / 越界 / NaN / Inf / 非数值。

## A.5 本批次累计统计

| 指标 | 前 | 后 |
|------|-----|-----|
| 单元测试数 | 671 | **690** (+19) |
| 失败 | 0 | 0 |
| `*n as usize` 直接出现在 source | 15+ | **0** (全部走 `usize_from_value`) |
| 重复的 `Int/Number/Float -> bounded` cast | 4 段 ~50 行 | **1 个泛型 helper** |
| `Int+Int` 算术溢出行为 | panic (debug) / wrap (release) | **`Err("integer overflow in addition")`** |
| `parse_budget_dispatch` 支持类型 | `Number` | **`Int` + `Number` + `Float`** |

### A.5.1 新增文件 / 新增 API

| 文件 | 新增 | 用途 |
|------|------|------|
| `src/flow.rs` | `pub fn usize_from_value` | 统一非负 `usize` 提取 |
| `src/checkpoint/mod.rs` | `fn bounded_uint_from_value<T>` | 统一 bounded 整数 cast |
| `src/interpreter/dispatch.rs` | `mod tests` (5 个回归测试) | 覆盖 `parse_budget_dispatch`/`int_from_value` |
| `src/checkpoint/mod.rs` | 6 个 `bounded_uint_*` 测试 | 覆盖泛型 cast helper |
| `src/flow.rs` | 6 个 `usize_from_value_*` 测试 | 覆盖 usize 提取 helper |

### A.5.2 仍未触及（按风险排序）

| 风险 | 位置 | 备注 |
|------|------|------|
| 🟡 | `src/document/backend/image.rs` 中的多平台 path 分支 | 已写明，建议文档化 |
| 🟢 | 测试代码中的 `panic!("expected ...")` | 几乎无害，可在别处提一个 nit PR |
| 🟢 | `flow.rs` 中残留"`v0.53 根因修复`"注释 8+ 处 | 建议保留 1 处权威说明，其余删除 |

---

# 附录 B：第三轮审计（v0.54 二批次 7 Bug 修复）

> 上一批次结束后用户再问"你还有什么没有修复的"，故继续扫荡 root cause。

## B.1 Bug 7 — `numeric_cmp` (Int, Int) 走 f64 精度丢失路径

### B.1.1 根因

`flow.rs::numeric_cmp` 接受 `Fn(f64,f64)->bool`，签名只覆盖浮点路径：

```rust
(Int(a), Int(b)) => Ok(Bool(op(a as f64, b as f64))),
```

v0.53 在 `eval_binary` Sub/Mul/Div/Mod 引入了 direct i64，但 `numeric_cmp`（即 `>`/`<`/`>=`/`<=` 的实现）**仍然**用 `op(a as f64, b as f64)`。f64 只能精确保留 < 2^53 (≈9e15) 的整数。

### B.1.2 真实 Bug

`numeric_cmp(Int(i64::MAX - 1), Int(i64::MAX), |a, b| a < b)`：

| 修复前 (走 f64) | 期望 |
|-----------------|------|
| 返回 `Bool(false)` | `Bool(true)` |

确认:failing regression test 失败，错误信息 `"numeric_cmp Int must use i64 direct comparison, got: Bool(false)"`。

### B.1.3 修复

- `numeric_cmp` 签名改为 `pub fn numeric_cmp<F, G>(left, right, f64_op: F, int_op: G)`，与 `numeric_op` 一致。
- `(Int, Int) → int_op(a, b)`，其余 `(Float, Float)` / `(Number, Number)` 等仍走 f64。
- `eval_binary` 的 4 个 `BinaryOp::{Greater,Less,...}` 调用同步更新为传 `int_op` 闭包。
- 5 个 numeric_cmp 测试同步迁移到新签名（其中 3 个原测试 + 2 个新回归）。

## B.2 Bug 8 — `execute.rs::parse_budget` 重复实现 + 同样漏 Int/Float

### B.2.1 根因

`execute.rs::parse_budget` 与 `dispatch.rs::parse_budget_dispatch` 字节级重复，且都仅匹配 `Value::Number`。Bug 4 我修了 dispatch，但 execute 的副本**没动**。

### B.2.2 修复

- 删除 `execute.rs::parse_budget` 与辅助 `split_number_unit`（后无他用）。
- `dispatch.rs::parse_budget_dispatch` 改为 `pub(crate)`。
- `execute.rs` 唯一调用点改为 `super::dispatch::parse_budget_dispatch(v, "budget")`，复用同一份支持 Int/Number/Float 的实现。

代码量净减少 ~60 行。

## B.3 Bug 9 — `execute.rs::max_tokens = Some(*n as usize)` 缺边界

### B.3.1 根因

```rust
"max_tokens" => {
    if let Value::Number(n) = v {
        cfg.max_tokens = Some(n as usize);  // 负数 -> usize::MAX
    }
}
```

且只接 `Value::Number`，`Int`/`Float` 字面量被静默忽略。

### B.3.2 修复

走 `usize_from_value` + 显式错误传播。同时把 `temperature` 从 `if let Number(n)` 升级为接受 `Int/Number/Float`，拒绝非数值。

## B.4 Bug 10 — `interpreter/mod.rs::mir_with_config` 第三份温度副本

### B.4.1 根因

`interpreter/mod.rs::mir_with_config` 也有同样的 `temperature`/`max_tokens` 重复 — 是第三份。前两轮没有扫到 MIR 桥。

### B.4.2 修复

同 Bug 9。

## B.5 Bug 11 — `interpreter/builtins.rs` 3 处 + ai_helpers.rs 1 处 `*n as usize`

### B.5.1 位置

| 位置 | 上下文 |
|------|--------|
| `builtins.rs:1089` (`ccr.marker` size) | `if let Some(Value::Number(n))` 然后 `*n as usize` |
| `builtins.rs:1800` (`mora.refine_info` iter) | 同 |
| `builtins.rs:2176-2177` (`exec.parallel` max_concurrent) | `(Number, Int) => (*n / *i as usize).max(1)` |
| `ai_helpers.rs:113/117` (`extract_usage` prompt/completion_tokens) | LLM API 返回的 usage 字段 |

### B.5.2 修复

全部走 `usize_from_value`：
- `ccr.marker` / `refine_info` 用 `unwrap_or(0)` / `Option<None>` 保留原 fallback 语义。
- `exec.parallel` 用 `?` 显式错误，因为 `max_concurrent` 是真实用户输入。
- `extract_usage` 用 `unwrap_or(0)` 保留 "API 不返回该字段 → 0" 的合理 fallback。

## B.6 Bug 12 — `mir/interp.rs::index_value` 3 处列表/字符串索引

### B.6.1 根因

```rust
(Value::List(list), Value::Int(i))   => { let i = *i as usize; ... }
(Value::List(list), Value::Number(n)) => { let i = *n as usize; ... }
(Value::String(s), Value::Int(i))    => { let i = *i as usize; ... }
```

MIR 解释器是 v0.42 引入的 **AST 并行解释引擎**，这条路径用户代码（`xs[-1]`）也会触发，但 AST 路径刚修，MIR 仍裸 `as usize`，绕开了修复。

### B.6.2 修复

合并 3 个 match arm 为 `(Value::List(list), _) => usize_from_value(idx, "List index")?` 与 `(Value::String(s), _) => usize_from_value(idx, "String index")?`，fallback 错误信息保持原样。

## B.7 Bug 13 — `parser_v2::orchestrate_statement::loop` 的 max_rounds 无负数防御

### B.7.1 根因

`src/parser_v2/statements.rs:845`:

```rust
let mut max_rounds = 10;
...
max_rounds = n as usize;  // n 是从 Token::Number(f64) 拿到，可负
```

来源是用户的 `orchestrate loop(name: x, max_rounds: -5)`。`-5 as usize` 静默换为 `usize::MAX - 4`，之后 orchestrate 引擎跑 ~10^19 轮。

### B.7.2 修复

加 `is_finite() && n >= 0.0 && n <= usize::MAX as f64` 三重检查，违反时按 parser 模块其它错误风格打 `eprintln!("Parse error: ...")` 并降级 `max_rounds = 0`，保持 parser 继续。

## B.8 Bug 14 — `compress::options_from_value` 3 处 `*n as usize`

### B.8.1 根因

`max_bytes` / `k_first` / `k_last` 都是 `usize`，来源是用户的 compress options dict（`compress(input, {max_bytes: -1})`）。走 `*n as usize` 同样静默换。

### B.8.2 修复

三处全部走 `usize_from_value` + `?` 错误传播。

## B.9 本批次统计

| 指标 | A 批次后 | B 批次后 |
|------|---------|---------|
| 单元测试 | 690 | **692** (+2: numeric_cmp max/eq) |
| 失败 | 0 | **0** |
| 编译警告 | 0 | **0** |
| `clippy -D warnings` | clean | **clean** |
| `cargo fmt --check` | clean | **clean** |
| 新增实现行 | — | **3 helper** (numeric_cmp signature, dispatch pub(crate), parser bounds) |
| 删除代码 | — | **~80 行** (`execute.rs::parse_budget` + `split_number_unit`) |

### B.9.1 新增公共 API

| 项 | 内容 |
|----|------|
| `pub fn numeric_cmp<F, G>(f64_op: F, int_op: G)` | 与 `numeric_op` 一致的双重闭包签名 |

### B.9.2 仍未触及

| 风险 | 位置 | 备注 |
|------|------|------|
| 🟢 | `src/ai_infra.rs:108/112/119` | 内部 API，由 config 提供 ratio，不在用户输入面 |
| 🟢 | `src/document/reading_order/mod.rs` | 文档 OCR 路径，已走 f64 → usize 单测 |
| 🟢 | `src/compress/{json,text}.rs` 的 `(... as f32) as usize` | 内部算法，由 ratios 驱动 |
| 🟢 | `src/lsp/providers/*.rs` | LSP 协议层，与 LSP server 自身状态耦合 |

---

# 附录 C：第四轮审计（v0.54 三批次 3 Bug 修复）

## C.1 Bug 15 — `interpreter/mod.rs::ai.embed` 索引无负数防御

### C.1.1 根因

```rust
let index = match m.get("index") {
    Some(Value::Number(n)) => *n as usize,
    _ => 0,
};
```

来源是 OpenAI / 自托管 LLM embedding API 返回的 `data[i].index` 字段。同样 `*n as usize` 取巧，无 Int / Float 支持，无负数 / NaN 防御。LLM API 返回 `index: -1` 会让 `(-1.0_f64) as usize = usize::MAX`，排序乱了。

### C.1.2 修复

走 `usize_from_value`，fallback 仍为 0（与原行为兼容）。

## C.2 Bug 16 — `document::reading_order_idx` 内部算法产物防御

### C.2.1 根因

```rust
if let Some(Value::Number(n)) = d.get("reading_order_idx") {
    Some(*n as usize)
} else {
    None
}
```

虽然 `reading_order_idx` 是算法内部 set 的，不直接来自用户，但若上层 dict 反序列化错误污染了字段（JSON 反序列化、用户手工构造 dict），仍会触发 wrap。

### C.2.2 修复

走 `usize_from_value`，错误视为 None（与"字典缺字段"语义对齐）。

## C.3 Bug 17 — `sandbox.containerize` cpu_cores / memory_mb 缺边界

### C.3.1 根因

```rust
if let Some(n) = args.get(3) {
    match n {
        Value::Number(v) => spec.limits.cpu_cores = Some(*v as u32),
        Value::Int(i)    => spec.limits.cpu_cores = Some(*i as u32),
        ...
```

用户从 `.mora` 调 `sandbox.containerize(cmd, ..., cpu_cores: -1)` 会让 `(-1_i64) as u32 = u32::MAX` 静默通过，传给底层 OS 容器 runtime 后果取决于平台：
- Linux cgroup: cpuset `-1` 解析失败，可能 panic 或返回 0 cpu；
- macOS sandbox-exec: `-1` 通常被忽略；
- 都不报错就静默吃掉。

### C.3.2 修复

将 `bounded_uint_from_value` 升级为 `pub(crate)`，在 `interpreter/builtins.rs` 复用：

```rust
spec.limits.cpu_cores = Some(
    crate::checkpoint::bounded_uint_from_value::<u32>(
        v, "sandbox.cpu_cores", u32::MAX as u64,
    )?,
);
```

`memory_mb` 同样走 helper，错误信息含字段名（"sandbox.memory_mb must be non-negative finite number, got: ..."）。

## C.4 本批次统计

| 指标 | B 后 | C 后 |
|------|------|------|
| 单元测试 | 692 | **692** (零回归；helper 已覆盖的语义复用) |
| 失败 | 0 | **0** |
| `cargo test --all` | clean | **clean** |
| `cargo clippy -D warnings` | clean | **clean** |
| `cargo fmt --check` | clean | **clean** |

### C.4.1 新增可见性

| 变化 | 内容 |
|-----|------|
| `bounded_uint_from_value` | 由 `fn` 升级为 `pub(crate) fn`，供 `interpreter/builtins.rs` 复用 |

### C.4.2 仍未触及（全面静态分析后最终残余）

| 风险 | 位置 | 备注 |
|------|------|------|
| 🟢 | `src/ai_infra.rs` 内部 ratio→usize | 由 config 提供 |
| 🟢 | `src/compress/{json,text}.rs` ratio→usize | 内部算法 |
| 🟢 | `src/lsp/providers/*.rs` line/col cast | LSP 协议层 |
| 🟢 | `src/document/reading_order/mod.rs:394,397,398,410` 像素坐标 | 文档 OCR 内部 |

---

# 终评

经过 v0.53 → v0.54 三轮审计 + 修复（共 17 Bug / 12 个用户脚本可触发的 root cause），用户输入面（`.mora` 脚本）的数值 → `usize` / `u32` / `u64` 路径已全部走 `usize_from_value` 或 `bounded_uint_from_value` 两个 helper：

| Helper | 位置 | 支持类型 | 拒绝条件 |
|--------|------|---------|---------|
| `usize_from_value` | `flow.rs` | `Int` / `Number` / `Float` | 负数 / NaN / Inf / 越界 / 非数值 |
| `bounded_uint_from_value<T>` | `checkpoint/mod.rs` | 同上 | 同上 + 目标类型 `TryFrom<u64>` |

代码层 root cause 收敛到 2 个公共 helper 之后，未来用户脚本触发的同类 Bug 应能在新增路径上**直接被 helper 阻止**，不再有"漏修一处"的可能。

**未触及的残余**（§C.4.2）均为内部算法产物或 LSP 协议层，与用户脚本输入无关，保留同评估即可。


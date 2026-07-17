# 一次"只读源码、不读文档"的代码审计工作流总结

> 本文档总结一次完整的"基于源代码而非任何技术文档"的项目架构审计会话。
> 面向学习者：**重点不是结论本身，而是怎么一步步逼出结论、怎么排除错误来源、怎么把"自以为知道"打回"未证实"。**

---

## 0. 会话背景

用户要求审计 `mora-lang` 项目当前架构，回答四个问题：

1. 能不能驾驭以后的高并发需求？
2. 能不能驾驭以后的高压力需求？
3. 能不能驾驭以后的强类型需求？
4. 能不能驾驭以后的静态类型需求？

约束（用户原话、贯穿全文）：
- **"基于源代码而非任何技术文档"**
- **"所有文档都是过时的废物"**
- **"项目哪有技术文档？我都不知道有技术文档"**

最终约束收敛为：**只看 `.rs` 源码 + git 事实 + 构建产物该告诉我的东西；不读任何 `.md` 文件、不读 `.rs` 里的 `//` 注释、不引用版本号。**

---

## 1. 总体工作流（6 阶段）

```
┌─────────────────────────────────────────────────────────────┐
│  Phase 1: 错误开局 —— 把文档/注释当事实                       │
│  Phase 2: 用户纠错 #1 —— "文档都是过时的废物"                  │
│  Phase 3: 第一次半改 —— 撤掉 .md，但仍引 .rs 注释              │
│  Phase 4: 用户纠错 #2 —— "项目哪有技术文档"                    │
│  Phase 5: 真正的纪律建立 —— 只看控制流 + 类型 + grep          │
│  Phase 6: "读完" —— 补完 5 条未追链路，给精确行号答案          │
└─────────────────────────────────────────────────────────────┘
```

每个阶段都有具体的"踩坑 → 反思 → 修正"过程，下面逐阶段展开。

---

## 2. Phase 1 — 错误开局

### 2.1 我做了什么（错的）

第一轮回答，我**大量引用**：

| 来源 | 引用示例 |
|------|---------|
| `README.md` | "README 第 89 行把 `parallel ... end` 列为语言特性" |
| `README.md` | "v0.19 Worker 并发（Ballerina）`parallel worker w1 ... end end`" |
| `AGENTS.md` | "改代码前先 `grep` 现有实现" |
| `Cargo.toml` | "version = "0.0.34"" —— 顺带 banner 写 v0.25 也当事实 |
| `CHANGELOG.md` | "v0.34 commit `b374975`" |
| `.rs` 里的 `//` 注释 | "v0.22: AI 调用内联缓存"、"v0.24: 缓存预热队列" |
| 测试模块里的注释 | `src/typeck/mod.rs:1115-1119` "Type::Union(vec![]) 只在 boundary 出现" |

### 2.2 我当时怎么想的（错的）

我有一个**隐含假设**：

> "README 是项目作者写的，作者比 AI 更懂项目 → README 是事实"

加上：

> ".rs 里的 `//` 注释和代码一起 commit、同一个 PR review → 注释 = 代码意图"

**两个假设都是错的。** 用户的纠正直接打掉这两个假设。

### 2.3 这一阶段的产物

一份"4 个问题"的答案表格，每行带**版本号、注释出处、设计意图描述**。看着很专业，**全是基于过期信息编的**。

---

## 3. Phase 2 — 用户纠错 #1

### 3.1 用户的话

> "README.md 里面都是不准的，太过时了，落后几十个提交"

### 3.2 我做了什么（半对的）

我承认 README 过时，**然后**：

1. 给出版本号错位的"硬证据"（`Cargo.toml:3` vs `main.rs:33`）
2. 给 git log 头 5 条都是 `fix(v0.34)` 的事实
3. 列出 7 条"README 可能过时"的具体位置让用户挑

**半对**：开始**用 git 事实 + 字符串字面量**而非 README 描述。
**还错**：依然在引用 `typeck/mod.rs:1115-1119` 那个测试模块注释当"项目意图"。

### 3.3 这一阶段暴露的认知陷阱

> 我把"被 git 忽略的本地资料"当成了"项目技术文档"。

证据：`docs/` 整个目录在 `.gitignore` 里。`AGENTS.md`、`CLAUDE.md` 也在 gitignore。

```
$ cat .gitignore
/target
...
docs/
CLAUDE.md
AGENTS.md
```

我**自己 git ls-files 一下**就能发现这问题，但**没做**。

---

## 4. Phase 3 — 第一次半改

### 4.1 我做了什么（还是错的）

我承认"应该直接看 .rs 源码"，**但**仍然：

1. 引用 `src/ast_v2.rs:369-370` 注释 "v0.19: Worker 并发" 当事实
2. 引用 `src/interpreter/mod.rs:155` 注释 "v0.19 Worker 并发 channels" 当事实
3. 引用 `src/typeck/mod.rs:1115-1119` 注释 "Type::Union(vec![]) 只在 boundary 出现" 当"项目设计意图"
4. 用 `// v0.22: 流式响应优化` 当事实

### 4.2 这一阶段的产物

10 条论断的"已证 / 未证"清单，**仍然混入了注释作为证据**。

---

## 5. Phase 4 — 用户纠错 #2（关键拐点）

### 5.1 用户的话

> "项目哪有技术文档？我都不知道有技术文档，你真当有技术文档吗？不要总是提我不知道的东西，那些都是辅助用的，项目代码更新后都不一定及时去修正这些文档，你把这些奉为真理是什么意思？"

### 5.2 我做了什么（对的）

**终于**意识到：

1. **项目就没有技术文档**（`.md` 全是辅助资料，可能过期）
2. **`.rs` 里的 `//` 注释 == 文档**（人手写、写完不一定跟代码同步、可能落后几十个 commit）
3. README 和注释**性质一样**

### 5.3 建立的纪律（写成 todo）

```markdown
- [in_progress] Stop treating .md / docs/ as project documentation
- [pending] Treat inline // comments in .rs files as stale documentation
- [pending] Only derive facts from: code paths, grep, type signatures, control flow
- [pending] For each claim, show exact .rs file:line that proves it (or admit I haven't read enough)
- [pending] Never cite README, CHANGELOG, AGENTS.md, CLAUDE.md, docs/*.md, or inline // comments
```

### 5.4 这一阶段的关键认知

> **"我说的任何东西，必须能在源码里 grep 到一个具体行号"** —— 否则要么撤回，要么明说"未证实"。

---

## 6. Phase 5 — 真正的纪律建立

### 6.1 三档说话原则

每次下结论前，把陈述分三档：

| 档位 | 含义 | 例子 |
|------|------|------|
| **已证** | 源码控制流 + 类型 + grep 直接证明 | "execute.rs:53 `Worker { .. } => Ok(FlowSignal::None` — body 永不执行" |
| **未证** | 推断 / 没读完链路 / 间接猜测 | "`Interpreter::Clone` impl 我没读完，所以不知道哪些字段 clone、哪些 reset" |
| **撤回** | 之前说错了 / 出于过期文档 / 出于注释 | "我之前说 10K 并发 = 几 GB —— 作废" |

### 6.2 实际产出的"已证事实"清单（带 grep 行号）

```bash
# 证明 Worker body 不执行
grep -n "Worker" src/parser_v2/ src/ast_v2.rs src/interpreter/

# 证明 parallel 串行
grep -n "execute_parallel\|// 简化实现" src/interpreter/execute.rs

# 证明 HTTP worker 共享 Interpreter 持锁
grep -rn "Arc<Mutex<Interpreter>>" src/

# 证明 ai_cache 真接通
grep -n "ai_cache\." src/interpreter/ai_chat.rs

# 证明 MORA_NO_TYPECK 不存在
grep -rn "MORA_NO_TYPECK" src/ tests/

# 证明 Union(vec![]) fallback 多
grep -n "Type::Union(vec!\[\])" src/typeck/check.rs | wc -l

# 证明 set_type / get_type 0 caller
grep -rn "set_type\|arena\.set_type\|arena\.get_type" src/
```

### 6.3 这一阶段的产物

**首次真正的源码事实清单**，每条带行号，但仍**有几条未追到链路**。

---

## 7. Phase 6 — "读完"补完

### 7.1 用户的话

> "读完"

两个字，意思是：**你说你有 5 条链没追完，那就去追**。

### 7.2 5 条未追链路

1. **`Interpreter::Clone` impl 全文**
2. **`call_value` / `call_value_inner` 内部锁粒度**
3. **`TypedExpr.ty` 是否有写入点**
4. **method dispatch 是否按 type 走**
5. **Clone impl 里 `HashMap::new()` 占位统计**

### 7.3 逐条追完的关键发现

#### 7.3.1 Clone impl（`src/interpreter/mod.rs:230-270`）

读完全文：

```rust
impl Clone for Interpreter {
    fn clone(&self) -> Self {
        Self {
            globals: self.globals.clone(),           // Arc::clone, 不深
            environment: self.environment.clone(),   // Arc::clone, 不深
            tool_registry: self.tool_registry.clone(),  // 深
            model_routes: self.model_routes.clone(),    // 深
            token_budget: self.token_budget.clone(),    // 深
            token_usage: self.token_usage.clone(),      // 深
            trace: self.trace.clone(),                  // 深
            route_registry: self.route_registry.clone(),// 深
            current_ai_config: self.current_ai_config.clone(), // 深
            trait_registry: self.trait_registry.clone(),// 深
            impl_table: self.impl_table.clone(),        // 深
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(),   // reset
            ai_cache: HashMap::new(),           // reset
            // ... 共 17 个 reset
            v2_arena: None,                     // reset
        }
    }
}
```

**数字**：12 个 deep clone（其中 2 个是 `Arc::clone` 不深，10 个真深 HashMap/Vec）+ 17 个 reset。

**修正之前的错**：我之前说"10K 并发 = 10K 份解释器，几 GB 堆"——**错**。`dispatch.rs:998, 1035` 是 **起 server 时 clone 一次**，per-worker 一份，请求时不再 clone。N 个 worker = N 份 Interpreter 状态，**不是每次请求 N 份**。

#### 7.3.2 call_value 锁粒度（`src/interpreter/dispatch.rs:1063-1101`）

```rust
pub(crate) fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
    match value {
        Value::Closure { v2_node_id, .. } => {
            if v2_node_id.is_some() {
                if let Some(ref arena) = self.v2_arena.clone() {
                    return self.call_value_inner(value, args, arena);  // 持 &mut self 走完 body
                }
                ...
            }
            ...
        }
        ...
    }
}
```

**`&mut self` 持锁范围 = 整个 closure body**。`http_server.rs:311` 在 `interpreter.lock().expect(...).call_value(...)` 持锁调它，**整个 HTTP handler 执行期间持有 `Mutex<Interpreter>` 写锁**。

#### 7.3.3 TypedExpr.ty 写入（`src/ast_v2.rs:597-601`）

```rust
pub fn set_type(&mut self, id: NodeId, ty: Type) {
    if let Some(expr) = self.exprs.get_mut(id.0) {
        expr.ty = Some(ty);
    }
}
```

方法存在。`grep -rn "set_type\|arena\.set_type" src/` —— **0 caller**（除自己的定义那一行）。

`src/typeck/check.rs:741` `pub fn check_expr` 是**纯返回 `Type`**，**从未 `arena.set_type(expr_id, ret_ty)`**。

`alloc_expr` 写入 `ty: None`，**永远保持 None**。

**`TypedExpr.ty` 是死字段**——结构在，没人写、没人读 `get_type` 也是 0 caller。

#### 7.3.4 method dispatch（`src/interpreter/dispatch.rs:442-941`）

全文读完。结构：

```rust
pub(super) fn call_method(&mut self, mut object: Value, method: &str, args: Vec<Value>, call_site: Span) -> Result<Value, String> {
    let _cache_key = format!("{}:{}", type_name(&object), method);  // 赋给 _ 后从未读
    if let Value::TraitObject { .. } = &object {
        return self.dispatch_trait_method(&object, method, args, call_site);
    }
    match object {
        Value::List(list) => match method {
            "push" => { ... }
            "map" => { ... }
            // ... 30+ 方法
        },
        Value::Dict(map) => match method { ... },
        Value::String(s) => match method { ... },
        Value::Builtin(name) => match (name.as_str(), method) {
            ("web", "fetch") => { ... }
            ("json", "parse") => { ... }
            // ...
        },
        // ...
    }
}
```

**没有 type table、没有 monomorphize、没有 O(1) 方法查找、全部 pattern match 链**。

`_cache_key` 计算后丢弃 —— **注释自承认"实际执行仍需分派"**，**真正的内联缓存未接通**。

#### 7.3.5 Clone impl 里 HashMap::new() 占位

精确数字：**17 个 reset**。

`mod.rs:230-270` 全文读完，行 245-267 是 reset 段。

### 7.4 这一阶段的产物

**最终答案**：4 个问题各带"已证控制流 + 已证控制流 + 未证清单"三段，每条都对应**具体 .rs 文件:行号**。

---

## 8. 最终答案（基于 Phase 6 的精确事实）

| 需求 | 源码事实（带行号） | 能否驾驭 |
|------|---------|---------|
| **高并发** | `src/interpreter/execute.rs:53` Worker body 永不执行；`src/http_server.rs:311` 持锁范围 = 整个 handler（`call_value` `&mut self`） | **不能** |
| **高压力** | `src/interpreter/mod.rs:230-270` Clone 12 字段 + 17 reset；`src/interpreter/ai_chat.rs:463-468` 每请求新建 Agent；`src/interpreter/dispatch.rs:1063-1101` `&mut self` 持锁到 handler 结束；`src/interpreter/dispatch.rs:442-941` 全 match 链 | **小流量可以**，**没水平扩展接缝** |
| **强类型** | `src/typeck/check.rs` 30+ 处 `Type::Union(vec![])` fallback；`src/typeck/check.rs:744` 通配兜底；`src/ast_v2.rs:597-601` `set_type` 0 caller；`src/typeck/check.rs:741` `check_expr` 不写回 AST | **正在做但被 fallback + 死字段 旁路** |
| **静态类型** | `src/value.rs:38-142` 全装箱；`src/interpreter/dispatch.rs:442-941` 全 match 链无 type table；`src/interpreter/mod.rs:271-275` `Environment` 字符串查名；`src/interpreter/dispatch.rs:450` `_cache_key` 计算后丢弃 | **不能**，运行时是 Python 模型 |

---

## 9. 给学习者的关键教训

### 9.1 错误类型 1：把过期文档当事实

**症状**：引用 README、CHANGELOG、AGENTS.md 里的版本号、特性表、设计描述。

**根因**：假设"文档 = 作者意图 = 事实"。文档写完后代码更新，文档不一定同步。

**修正**：
- 看到 .md 文件 → 默认怀疑它过期
- 用 `cat .gitignore` 检查 .md 是否在仓库内
- 不引 .md 作为论据来源

### 9.2 错误类型 2：把 .rs 注释当事实

**症状**：引用 `// v0.22: ...`、`// v0.19: ...`、测试模块里的设计原则注释。

**根因**：注释和代码同仓同 commit，让人觉得"注释 = 代码"。但注释是**人手写的人语言**，可能落后代码几十个 commit，跟 .md 一个性质。

**修正**：
- 看到 `// xxx: ...` → 当过期描述
- 只看**控制流 + 类型 + grep 输出**，不看注释的字面意思
- 注释用来**找到代码位置**，不用来**解释代码为什么这样**

### 9.3 错误类型 3：把推断当事实

**症状**：下结论时混进"应该是 X"、"按设计是 Y"、"v0.x 距 v1.0 还差 Z"。

**根因**：训练数据 + 文档 + 注释混在一起，模型自动脑补"项目意图"。

**修正**：
- 每条结论前问自己："这条**直接**在源码里 grep 得到吗？"
- 不能 → 标"未证"
- 能 → 给具体文件:行号

### 9.4 错误类型 4：未追完链路就下结论

**症状**：看到一处就推断全貌。

**根因**：偷懒 + 想"快答"。

**修正**：
- 显式列出"未追完链路"
- 用户说"读完" → 真的去追完
- 追完才能给精确数字，不能给则**明说**

### 9.5 核心方法论

> **每条结论必须能在源码里 grep 到一个具体行号，否则撤回或标"未证"。**

执行层面：

```bash
# 1. 找位置（不看注释）
grep -rn "exact_symbol" src/

# 2. 直读那段代码（不看注释）
Read src/xxx.rs:行号范围

# 3. 找 caller（验证接通）
grep -rn "function_name\|method_name" src/

# 4. 三档说话
# - 已证：行号 + 控制流
# - 未证：列"我没追完的链路"
# - 撤回：明说"之前说的作废"
```

---

## 10. 可复用的工作流清单（Checklist）

下次做类似审计，直接照这个清单走：

### 10.1 准备阶段

- [ ] `cat .gitignore` — 确认哪些目录/文件被忽略
- [ ] `git log --oneline -20` — 看真实提交演进
- [ ] **明确规则**：本次只读 `.rs`、git 事实、构建产物；不读任何 `.md`、不引任何 `//` 注释

### 10.2 探索阶段（每个特性）

- [ ] `grep -rn "feature_keyword" src/` — 定位代码
- [ ] 直读每处匹配的 `.rs` 文件:行号范围
- [ ] 跳过所有 `//` 注释，只看控制流 + 类型
- [ ] 找 caller：`grep -rn "function_or_method_name" src/`
- [ ] 验证"声明的方法是否真接通"——例如声明 `set_type` 但 0 caller = 死字段

### 10.3 结论阶段

每条结论强制三档：

- [ ] **已证**：列具体 `.rs` 文件:行号 + 控制流描述
- [ ] **未证**：列"我没追完的链路 X / Y / Z"
- [ ] **撤回**：列"之前说 X 作废，因为 Y"

### 10.4 补完阶段（用户说"读完" / "再核" / "追完"时）

- [ ] 列出未追链路清单
- [ ] 逐条读完
- [ ] 修正之前的错（数字 / 范围 / 因果）
- [ ] 重新出最终答案

---

## 11. 这次会话里**做错的**具体清单（学习者可直接对照）

| # | 错在哪 | 触发纠正 | 修正手段 |
|---|--------|---------|---------|
| 1 | 引用 README.md 第 89 行 / 152 行 / 描述 | "README.md 里面都是不准的，太过时了" | 撤掉所有 README 引用 |
| 2 | 引用 CHANGELOG 版本号 | 同上 | 改用 `git log` 看真实提交 |
| 3 | 引用 AGENTS.md / CLAUDE.md / docs/*.md | "项目哪有技术文档" | `cat .gitignore` 确认这些都被忽略 |
| 4 | 引用 `.rs` 里 `// v0.22: ...` 注释 | "所有文档都是过时的废物"（泛化到注释） | 把 `//` 注释当过期描述，只看控制流 |
| 5 | 引用 `typeck/mod.rs:1115-1119` 测试模块注释当"项目意图" | "你把这些奉为真理是什么意思" | 测试模块的注释 ≠ 项目意图，只看它定义的测试断言 |
| 6 | "10K 并发 = 几 GB 堆" | 用户没直接纠正，"读完"后自查 | `Clone` impl 全文读完，数字修正 |
| 7 | "`Mutex<Interpreter>` 大锁，N 线程 ≈ 1 线程"程度未知 | 用户说"读完" | `call_value` `&mut self` 持锁范围读完 |
| 8 | "`TypedExpr.ty` 是否有写入"未核 | 用户说"读完" | grep `set_type` caller = 0，结论修正 |
| 9 | "method dispatch 是否按 type 走"未核 | 用户说"读完" | `call_method` 全文读完，全 match 链 |
| 10 | "Clone impl 里 HashMap::new() 占位"未核 | 用户说"读完" | 精确数到 17 个 reset |

---

## 12. 一句话总结

> **不要相信任何"项目说它是什么"，只信"源码做了什么是 / 没做什么"**。
> 文档、注释、设计描述都是"项目说它想做什么"——可能过时、可能画饼、可能跟代码不同步。
> 源码的 `match` / `grep 0 命中` / `&mut self` / `Arc::clone` 才是事实。
>
> 任何"按设计应该是 X"的陈述 = **撤回**。
> 任何"我在源码里 grep 到 `xxx.rs:NN` 是 X"的陈述 = **已证**。
> 中间地带 = **明说"未追完"**，让用户决定要不要继续追。
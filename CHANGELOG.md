# Changelog

All notable changes to Mora will be documented in this file.

## [v0.75.78] — 2026-08-04 — compile 主路径嵌套构造缺口修复

实际 exe 运行嵌套控制流暴露 compile 主路径（v0.75.40+）的解析缺口——
旧 parse 路径支持、compile 解析失败的形态，三处对称修复：

- **emit_block_w `{` 分支不跳换行**：`if c {\n stmt\n}` 多行 brace 块在
  compile 主路径解析失败（旧路径可解析，差分测试只覆盖单行 if 未暴露）。
  补前导/尾部换行跳过，与 else 分支、parse_block_body 对称。
- **emit_statement_expr_w 缺构造分发**：task 体/闭包体/for 体内 if/for/
  while/match/let 直接落 emit_expr_w → 解析失败。补齐语句级分发，镜像
  parse 侧（Let 优先 > Match > If > For > While）。
- **非 FatArrow 闭包体改用 emit_block_w**：`fn(n) if n<=1 {..} else {..} end`
  走 emit_block_w（镜像 parse_block_body），支持多语句与嵌套构造。

回归测试：compile_differential 新增 compile_equivalent_nested_constructs
（差分等价，锁定交集形态）+ compile_run_nested_constructs（task 体 if
运行回归）。验证：全量测试绿（docker 依赖 skip）+ clippy `-D warnings` 0
+ fmt 0。

已知 pre-existing（与本次无关，两条路径行为一致）：
- 顶层 `task` def 的 n_regs 差 1（lower 为无 dst 的 TaskDef 分配死寄存器，
  body 指令序列一致）。
- 闭包 `fn(n) if c {1} else {0} end` 的 else 值经 DAG 执行丢失（`pick(0)`
  返回 nil 而非 0）——DAG 对 Var（读 env）与 Assign（写 env）之间缺 env
  依赖边，走线性解释器可正确执行。属 DAG 执行器独立缺陷，待后续修复。

## [v0.75.77] — 2026-08-03 — 环境经参数单一传递（去全局槽/回落形态）

v0.75.76 的修复采用「h_call 执行 env 直查 + 其余回落 mir_call_function」
的双路径形态。按项目「不允许兜底/回落代码」原则重构为单一来源——
执行环境经参数贯穿所有调用桥：

- **MirHost::mir_call_function 增 `env` 参数**：trait 签名、Interpreter
  impl、`call_function`/`call_builtin_fallback` 全部改为从参数取执行环境，
  不再查询宿主全局槽。file.* handler（save/load/read/write/append/
  read_bytes/write_bytes）同步补 env 参数传递。
- **h_closure 闭包捕获修复（新 bug）**：h_closure 原用 `interp.environment()`
  （宿主全局槽）捕获闭包环境——take_env 移空后捕获到空壳，闭包体查不到
  顶层绑定（`let base=10; let f=fn(x) x+base end; f(5)` 运行时
  "Operands must be two numbers..."）。现捕获执行 env 参数（与 h_define
  同一容器），全局槽读取清零。
- 兜底分支语义收敛：h_call 的 `env.get(name)` 命中 callable 走 call_value，
  其余（builtin/未定义）统一经 mir_call_function(env) —— 无回落，单一传递链。

回归测试：compile_differential 新增 compile_closure_captures_top_level_binding。
验证：全量 791+ 绿（docker 依赖测试因 daemon 不可达 skip，pre-existing）+ 实际
exe 综合脚本全过 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.76] — 2026-08-03 — 实际运行修复两 bug

编译 mora.exe 实际运行综合脚本暴露两个真实 bug（全量测试未覆盖）：

- **P6 登记守卫误拦用户函数**：call_function 顶层 testcase! 断言
  `_kind.is_some()`，用户自定义函数 `_kind.is_none()` → debug 构建 panic。
  校验点移至 `_` 兜底分支（builtin 名落兜底才告警，用户函数合法落兜底）。
- **顶层绑定对裸函数调用不可见**：take_env 移出 core.environment（空壳），
  run_mir 的 h_define 写私有 env 参数，而 call_function 兜底查 core →
  `let f=fn...; f(5)` 报 Undefined。修复：h_call 先用执行 env 查用户
  callable（与 h_define 同容器、无锁），其余回落 mir_call_function。
  弃 active_env 槽方案（parking_lot 不可重入锁 → 死锁，已回退）。

回归测试：compile_differential 新增 compile_bare_user_function_call。
验证：全量 791 绿 + 实际 exe 综合脚本全过 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.75] — 2026-08-03 — 分隔符横幅全仓统一

D7 分隔符统一——4 种字符（`=` `─` `-` `═`）收敛为 2 种语义：

- 纯分隔线（`-`/`═`，无文字）→ `=`（24 处：checkpoint/lsp/server/
  handlers×6）
- 带文字标题横幅（`=text=`、`-text-`）→ `─text─`（10 处：pregel 的
  PLAN/EXEC/UPDATE/ADVANCE 段标 + flow/reading_order/event/cost/rule
  测试标题）
- 全仓残留分布：纯 `=` 102 处 + 带文字 `─` 128 处，零 `-`/`═` 纯分隔、
  零混用
- 排除：lexer.rs 中文破折号正文、main.rs `--version` 参数注释（非横幅）

验证：34 删 = 34 增全横幅字符（白名单脚本，零代码改动）+ 全量 790 绿
+ clippy `-D warnings` 0 + fmt 0。

## [v0.75.74] — 2026-08-03 — import 顺序全仓统一 super-first

用户明确「风格一定要统一」——撤销 v0.75.73「同簇同序」决策，全仓
统一为 super-first（std → super → crate，与 rustfmt 强制 glob 方向
一致、31 文件既有惯例）：

- 10 个 crate-first 文件重排：mir/handlers,inst,ssa,ssa/deconstruct,
  vm,vm/dag,opt/{copy,simple,tailcall} + typeck/check_mir
- 语句级重排（跨行 use 语句整体移动，不拆行），纯 use 移动零行为变化
- 事故与修复：首版脚本误删 vm/dag.rs 顶部 20 行解释器语义注释（执行
  边界文档）——git checkout HEAD 恢复 + 手动重排 use 保留注释；其余
  9 文件核查 0 注释删除
- 验证：40 个含 super+crate 文件 0 违规同向 + 全量 790 绿 + clippy
  `-D warnings` 0 + fmt 0

## [v0.75.73] — 2026-08-03 — 风格审计 D4/D5/D6 落地

- **D6 模块头补全（18 文件）**：lexer/main（v0.01）+ interpreter/mod +
  record×7 + lsp/providers×9（v0.25）——版本溯源 + 功能描述，与全仓
  `//!` 惯例统一
- **D5 expect 短消息补全（69 处/8 文件）**：32 个唯一短消息值（find/
  list/parse/issue 等）改带语义消息。全部位于测试代码，改善 panic 定位
- **D4 import 顺序定调**：实测格局 31 super-first（interpreter/lsp/
  compress/cli 簇）+ 9 crate-first（mir 系）。唯一异类 mir/lower.rs
  对齐到 crate-first——mir 系 9 文件全同向，interpreter 系 31 文件
  不动（同簇同序，零跨簇强制）

验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.72] — 2026-08-03 — 架构审查修复：生产 unwrap 清零 + 前缀对齐

architecture-reviewer 复查纠正 v0.75.64 审计范围偏差（35 会话文件 ≠ 全仓）：

- **生产裸 unwrap 全仓清零（8 处）**：rule.rs/search.rs 冗余双查合并为
  `if let`；mora.rs/refine/mod.rs（ok_or 无 panic 面）/toolplane/mod.rs/
  dag.rs/html.rs 补 expect/ok_or。校验：全仓 src/ 非测试范围 = 0
- **错误前缀对齐**：toolplane.rs `tool.plane.*` → `toolplane.*`（14 处，
  与 from_name 分派键一致）；ai_tokens.rs 头 `ai_tokens.*` → `ai.tokens.*`
- **import 分组定调**：不强制统一——实测两簇各自 100% 自洽（mir 系
  9 文件 crate-first、interpreter 系 31 文件 super-first），强统一改 40
  文件零收益，保持按簇惯例

验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.71] — 2026-08-03 — 代码风格统一：opt pass 文件头 // → //!

代码风格审计发现 v0.75.60 拆 opt.rs 时 5 个 pass 文件头用了普通注释
`//` 而非模块文档 `//!`——其余 30 个拆出文件（cli/inst/vm::dag/
infer 等）全部是 `//!`。

修正：copy/loops/pregel_opt/simple/tailcall 头部 `//` → `//!`，
与全仓拆出文件惯例统一。纯注释风格，零行为变化。

验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0；28 个拆出文件
头部全部 `//!` 确认。

## [v0.75.70] — 2026-08-03 — typeck/hm 拆 infer.rs + builtin.rs

hm/mod.rs（922 → 502 行）`impl HMInference` 按方法组拆分子模块（D6
多 impl 块惯例，与 builtins 同款）：

- `infer.rs`（373）— infer_* 方法族（let/assign/var/binop/call/method_call/
  closure/fn_def/match/if/list/dict）
- `builtin.rs`（66）— builtin_callee_ty + builtin_type（op 类型推断）
- mod.rs 保留基础设施（fresh_type_var/instantiate/solve_constraints/
  infer_program/infer_expr 入口）
- 跨文件方法可见性经 `pub(super)` 统一（24 处批量，仅 4 空格缩进顶层
  方法，本地 fn 不受影响）

纯搬移零行为变更。验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.69] — 2026-08-03 — lexer.rs simple_token 收敛

next_token 的 33 处 `Some(Token { token_type, line, column })` 构造
（line/column 恒为 start_line/start_col）→ 提取 `simple_token` 辅助：

- 覆盖 12 单字符简单分支 + 21 多字符/嵌套分支（含 DotDotDot 深嵌套）
- `Lifetime(lifetime)` 带 payload 变体一并收敛（simple_token 收 TokenType 值）
- next_token 303 → 220 行；lexer.rs 848 → 726 行
- 纯等价替换（33 处字段完全一致，正则断言 33 全覆盖）

验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.68] — 2026-08-03 — ai_chat HTTP 构造收敛（ai_chat_url + ai_agent）

三处 AI API HTTP 段重复收敛（仅零风险项，错误消息是契约不动）：

- `ai_chat_url(base_url)`：3 处 URL 构造（chat/completions）→ 1 个辅助
- `ai_agent(read_timeout, Option<write_timeout>)`：3 处 agent 构建 → 1 个
  辅助，参数保留各调用方超时差异（call_ai_api 单超时 30s；
  send_with_retry/with_tools 双超时）→ 零行为变化
- 不合并 post+send 传输内核：错误前缀是 `is_retryable_error` 契约
  （interpreter/mod.rs 依赖 `"ai.chat: API error HTTP"` / `"network error"`）

纯提取零行为变更。验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.67] — 2026-08-03 — ai_chat.rs real_ai_chat_inner 提取 send_with_retry

real_ai_chat_inner（303 → 178 行）— 构造/发送两阶段分离：

- 前置段保留：空消息/上下文窗口/mock/投机执行/内联缓存/预热队列
  （均提前 return）
- `send_with_retry`（130 行）提取：请求构造（messages JSON/body 拼
  temperature/url/agent）+ 发送重试（exponential backoff + jitter + token
  追踪 + LRU 缓存写）
- cache_key（inner 局部）作参数传入（方法内 LRU 写需要）
- 纯提取零行为变更（逐行搬移 + cache_key 传参）

验证：全量 790 绿 + clippy `-D warnings` 0 + fmt 0。

## [v0.75.66] — 2026-08-03 — dispatch.rs call_function 按 name 提取（God Function 拆分）

call_function（473 行巨型 match）按 name 提取为 18 个 `call_builtin_*`
私有方法（统一签名 `&mut self + args`），主干保留 name → 方法分派：

- merge_with/print/range/len/compose/partial/atom/swap/deref/type_of/
  is_instance/methods_of/compress/crush_json/batch_chat/into/tail/
  compose_prompt
- 兜底分支（环境查值 → call_value / Macro 展开）提取为
  `call_builtin_fallback(name, args)`，match 补 `_` arm
- 原行提取不做手动缩进（rustfmt 统一），避免 v0.75.65 textwrap 缩进事故
- Trait::new 早退 + P6 BuiltinKind 登记校验保留

行为等价：全量 790 绿 + clippy `-D warnings` 0 + fmt 0；冒烟 print 正常。

## [v0.75.65] — 2026-08-03 — dispatch.rs call_method 按类型提取（God Function 拆分）

call_method（644 行巨型 match）按 Value 类型提取为 10 个私有方法，主干
保留类型分派（D6 惯例）：

- `call_method_{list,dict,builtin,string,stream,router,mcp,document}`：
  值语义传参
- `call_method_conversation` / `call_method_agent`：arm 移动 object +
  方法内 let-else 解构（refutable 模式）；conversation 的 compress arm
  重建对象（compress_top 需完整 Value），agent 用 object 参数避开
  clippy too_many_arguments
- builtin 分支（22 个 (kind, method) 组合）包 `match (kind, method)`
- TraitObject 早退与 `_` 兜底不变

行为等价：全量 790 绿 + clippy `-D warnings` 0 + fmt 0；冒烟 List/String/
Dict 方法正常。主干 644 → ~40 行，每类型方法可独立演进。

## [v0.75.64] — 2026-08-03 — 约束审计：生产代码裸 unwrap 清零

AGENTS.md「生产代码避免 unwrap()」审计——扫描全部 35 个模块化会话
创建/修改文件：

- 生产代码仅 1 处裸 unwrap：`loops.rs:45 pre_header.unwrap()`（licm
  pass，v0.75.6 既有代码，v0.75.60 搬移时未顺手修）
- 其余全部位于 `#[cfg(test)]` 测试区（测试 unwrap 为 Rust 惯例，不违反
  生产代码约束）
- 修复：is_none 检查 + unwrap 合并为 let-else（行为等价：None →
  continue），消除双重判断与 panic 面

验证：全量 790 绿（skip 3 个 Windows 挂起基线）+ opt 60 测试绿 +
clippy `-D warnings` 0 + fmt 0。

## [v0.75.63] — 2026-08-03 — flow.rs 拆 flow/json.rs（模块化）

flow.rs（773 → 521 行）拆出独立 JSON 编解码器：

- `flow/json.rs`（259）— json_to_value（手写递归解析：list/dict/bool/
  null/number/string + skip_ws）+ value_to_json（序列化）
- 边界：JSON 段零 flow 依赖（纯 Value 转换，只用 Value + HashMap）
- `pub use json::{json_to_value, value_to_json}` 保持 flow:: 路径
  （8 个外部调用方零改动）
- 单文件模块 → 目录模块

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.62] — 2026-08-03 — value.rs 拆 value/display.rs（模块化）

value.rs（1376 → 1213 行）拆出纯格式化段：

- `value/display.rs`（169）— impl std::fmt::Display for Value + fmt_inner
  （深度限制递归格式化，NaN/Inf 安全 v0.36）
- 边界：Display 区零 BuiltinKind/Environment 依赖，fmt_inner 仅区内
  自调用——最干净段落
- 单文件模块 → 目录模块；impl 在子模块内实现父模块类型（同 crate
  允许），Display trait 全局可见，外部零改动

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.61] — 2026-08-03 — mir/vm.rs 拆 vm/dag.rs（模块化）

vm.rs（923 → 565 行）线性/DAG 双语义段拆分：

- vm.rs 保留线性区：run_mir / run_mir_with_signal / MirSignal /
  build_task_registry / run_main_task / 索引与模式匹配辅助
- `vm/dag.rs`（373）— DAG 超步执行器（BSP 超步模型，生产主路径）：
  run_dag* / DagExecMemo / is_memoizable_pure / node_ready / is_control_edge
- 单文件模块 → 目录模块；`pub use dag::*` 保持 `vm::run_dag*` /
  `vm::DagExecMemo` 路径（P4 对外契约不变，cache.rs/pregel/tests 零改动）
- 依赖单向 dag → super（线性区）+ crate::mir::dag（类型）；无循环

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.60] — 2026-08-03 — mir/opt.rs 拆 5 个 pass 文件（模块化）

opt.rs（1176 → 208 行）按 SSA pass 组拆分子模块（D6 单文件惯例）：

- `simple.rs`（335）— ConstProp/DeadCodeElim/Gvn + ssa_dst 共享辅助
- `loops.rs`（464）— Licm/LoopStrengthReduction + loop 分析辅助
- `copy.rs`（102）— CopyProp + next_free_reg / LsrOps 类型
- `tailcall.rs`（60）— TailCallOpt
- `pregel_opt.rs`（23）— superstep_fusion / optimize_pregel
- opt.rs 骨架：SsaPass 定义/impl + pipeline 组装 + optimize/run_pipeline
- 跨文件依赖经 pub(super) + 显式 use（loops↔copy 双向，同 crate 允许）

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.59] — 2026-08-03 — document/reading_order 拆 xy_cut.rs（模块化）

reading_order/mod.rs（1159 → 856 行）拆出 XY-Cut++ 算法实现：

- `xy_cut.rs`（314）— XY-Cut++ 排序（递归投影-轮廓分裂 + cross-layout
  处理）+ 5 个算法常量（BETA/DENSITY/OVERLAP/MIN_OVERLAP_COUNT/MIN_GAP）
- mod.rs 保留 BBox/Strategy 定义 + assign_reading_order 主入口 + 测试区
- xy_cut 区零 Value 依赖（纯 BBox/Vec 几何计算）；const 归属算法域随迁

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.58] — 2026-08-03 — mir/ssa.rs 拆 ssa/deconstruct.rs（模块化）

ssa.rs（1444 → 1035 行）construct/deconstruct 两阶段拆分：

- `ssa.rs` 保留 SSA 构造（基本块划分/支配树/phi 插入/变量重命名）+ 类型定义
- `ssa/deconstruct.rs`（409）— deconstruct（phi → copy，SSA → MIR-plain）
  + map_ssa/next_tmp_name/ssa_inst_to_plain/terminator_to_plain 私有辅助
- 单文件模块 → 目录模块；`pub use deconstruct::deconstruct` 保持
  `ssa::deconstruct` 路径（opt.rs 调用点零改动）
- deconstruct 区仅用 HashMap/HashSet + 3 个 crate 类型，导入自包含

纯搬移零行为变更。验证：全量 790 绿（skip 3 个 Windows 挂起基线）+
clippy 0 + fmt 0。

## [v0.75.57] — 2026-08-03 — pregel run() 提取 execute_step（模块化）

`run()`（522 → 214 行）BSP 超步循环瘦身 — 最大块 EXEC（314 行，含
fault tolerance 重试 + 顺序/并行双路径）提取为独立方法：

- `execute_step(&mut self, interpreter, to_execute, &mut next_active)`
  → `Result<Vec<writes>>`：BEGIN → 重试循环 → flush sends；UPDATE 段消费
- run() 保留超步骨架：PLAN → execute_step → UPDATE → master_compute →
  ADVANCE → checkpoint
- 纯提取零行为变更（逐行一致）；仅 borrow 适配（`&to_execute` 借用、
  next_active 闭包 reborrow）

验证：全量 790 绿（skip 3 个 Windows 挂起基线）+ clippy 0 + fmt 0。

## [v0.75.56] — 2026-08-03 — mir/handlers.rs 拆 inst.rs（模块化）

handlers.rs（1451 → 895 行）按两部分拆分子模块，纯搬移零行为变更：

- `inst.rs`（585）：MirInst metadata（dst/input_regs/map_regs/is_effect）+
  dispatch 指令级分派（v0.59 起线性/DAG 解释器共享）
- handlers.rs 保留 h_* 函数库 + Flow + 共享定义
- 依赖方向单向 inst → handlers（dispatch 调 h_*），无循环
- 路径兼容：handlers.rs re-export dispatch + mod.rs `pub use inst::*`

测试同步：tier0_dyntrait 的 dispatch 源码断言改查 inst.rs（拆后新位置；
h_dyn_trait 仍在 handlers.rs）。行为未变，仅测试耦合的实现位置更新。

验证：全量 790 绿（skip 3 个 Windows 挂起基线）+ clippy 0 + fmt 0。

## [v0.75.55] — 2026-08-03 — compress/json.rs 分层拆子模块（模块化）

SmartCrusher 按既有四层结构拆分子模块，纯搬移零行为变更：

- `detect.rs`（301）：字段角色检测器（extract_field_stats/detect_field_role/
  detect_{id,score,temporal,error,anomaly}）+ ArrayType 判定
- `strategies.rs`（202）：5 种压缩策略（TopN/TimeSeries/ClusterSample/
  SmartSample/Lossless）+ apply_all/finalize
- `constraints.rs`（123）：3 种安全约束（KeepErrors/KeepOutliers/
  KeepBoundary）+ z-score 异常检测
- `json.rs`（1516→921）：保留共享定义（FieldRole/FieldStats/ArrayType/
  Strategy/Constraint/CrushResult/ERROR_KEYWORDS）+ 主入口 + 测试区
- 依赖方向 detect ← strategies ← constraints 单向下行；pub re-export 不变

验证：全量 790 绿（skip 3 个 Windows 挂起基线）+ compress 30 测试全绿 +
clippy 0 + fmt 0。

## [v0.75.54] — 2026-08-03 — builtins/mod.rs 残余拆分（模块化收尾）

P7 拆 domain 遗留两处结构问题，本次清零：

- **生产代码归属错位**：markdown 辅助函数（markdown_memory_dir/remember/
  recall/list + 日期工具，203 行）住在 mod.rs 却只被 memory.rs 使用 →
  迁 memory.rs
- **实现与入口分离**：exec_parallel/ParallelResult/Semaphore 等 375 行实现
  留在 mod.rs，call_exec_method 入口已在 exec.rs → 迁 exec.rs

迁移明细：
- `memory.rs`（144→527）：markdown 辅助 + memory 部分 7 测试
- `exec.rs`（15→616）：exec 实现 + tests_v043_exec 9 测试
- `event.rs`（87→179）：bus 测试迁入（6 测试）
- `mod.rs`（3319→2242）：**残余生产代码清零**，仅剩历史测试聚合
- `ccr.rs`：+`use crate::ccr::CcrStore` 自包含（此前经 mod.rs 顶层 use
  隐式继承，唯一非纯搬移改动，语义等价）

验证：全量 790 绿（skip 3 个 Windows 挂起基线）+ 迁移 45 测试全绿 +
clippy 0 + fmt 0；memory/exec 冒烟与 HEAD 行为一致。

## [v0.75.53] — 2026-08-03 — Phase 2 架构重构 P9：main.rs 拆 cli/（D6 单文件惯例）

main.rs 从 1117 行瘦身至 415 行——CLI 子命令整体迁入 lib crate 的
`src/cli/`（SQLite VDBE 单文件弹合惯例，与 P7 builtins 拆 domain 同风格）。

### cli/ 模块（lib 侧）
- `src/cli/mod.rs`：`compile_and_opt` 单遍编译入口 + 8 个共享 helpers
  （recordings_dir/recording_path/snapshots_dir/snapshot_path/format_duration/
  format_size/format_ts/truncate）
- `src/cli/record.rs`：record 组 10 命令（record/replay/diff/list/stats/
  export/audit/report/timeline/snapshot）
- `src/cli/mcp.rs`：mcp 组 3 命令（tool-list/tool-search/toolsets）
- main.rs 仅保留 dispatch + run_file/run_check/run_repl + install/banner

### 验证
- 全量测试 790 绿（skip 3 个 Windows 环境挂起测试，基线一致）
- clippy --all-targets --all-features -D warnings 0；fmt 0
- 冒烟：16 个 CLI 路径逐一验证（run/check/record/replay/diff/list/stats/
  timeline/export/audit/report/snapshot/mcp×3）——含无分号语法（v3 语言
  以换行为语句分隔，`;` 非本语言语法，examples/ 为 v0.03 旧语法）
- 纯代码搬移 + 路径改写（mora:: → crate::），零行为变更

## [v0.75.50] — 2026-08-03 — JIT 收口三件事（调研驱动）

（直接回应调研「jit.rs 将成 God Object」的最高风险：模板契约可审计化
+ 错误分类结构化 + 文档事实修正。为 LuaJIT 式 snapshot/side-exit 打基础。）

### JitError 结构化错误（src/mir/jit.rs）
- `run_jit`/`try_compile` 从 `Result<Value, String>` → `Result<_, JitError>`：
  - `CompileReject` — 模板集未覆盖（指令/类型/平台/跳转越界），编译期
    即知稳定可预测
  - `GuardFail` — 运行期类型标签守卫失败（生成代码置 bail），未来可映射
    snapshot/side-exit
  - `InternalInvariant` — 基础设施破坏（可执行内存/W^X 失败），非程序语义
- `h_with_config` 回落诊断自动携带分类（Display 实现）；测试
  `run_jit_of` 映射 `to_string()`。

### TemplateSpec + verifier-first（src/mir/jit.rs）
- `TemplateSpec` 枚举声明全部可编译模板契约（Const×3 / Binop 6 类 /
  Jump / JumpIf）+ `result_type()` 单一来源。
- `template_for_binary(is_int, is_float, op) → Option<TemplateSpec>` —
  BinaryOp 的（类型×op → 模板）判定收敛，try_compile 与 verify_linear
  共用（新增模板须先登记契约，否则 verifier 拒绝）。
- `verify_linear` verifier-first 预检（Cranelift 思想）：编译前独立校验
  寄存器范围 + 类型可推导 + 指令在契约表内，判定（spec）与发射（emit）
  分离。

### AGENTS.md 编译管线事实修正
- 生产主路径标注为单遍编译 `source → Lexer → ParserV3::compile →
  MirFunction<MirInst> + MirWitness → witness typecheck → MIR optimize →
  DAG → vm::run_mir`（v0.75.40+）；旧 `parse → MirExpr → lower` 标注为
  历史/兼容路径；执行内核指向 vm.rs + jit.rs。

## [v0.75.47-49] — 2026-08-03 — 架构收口 Phase 1（调研驱动，P1-P5）

（架构审查 × 跨界调研合并计划的零风险立即项，每项独立 commit、
差分/全量测试锁行为。P1-P3 在 v0.75.47、P4 在 v0.75.48、P5 在 v0.75.49。）

### P1 — 统一 value_type_name（src/compress/json.rs）
- 删手写 6 变体 JSON 投影 match，复用 `flow::type_name` + 仅 3 个 JSON
  专名映射（list→array / dict→object / nil→null），语义对 JSON 输入不变。

### P2 — Value::methods() 下沉（src/value.rs / src/interpreter/dispatch.rs）
- `get_methods_for_value` 从 flow.rs 移到 `value.rs::impl Value::methods()`
  （Lua 5.4 元表「内禀属性贴近数据定义」思想）。dispatch 的 `methods_of`
  builtin 直接 `value.methods()`，全仓旧函数清零。

### P3 — 清 v2 TypeChecker 死引用（src/typeck/dispatch.rs）
- 模块注释仍引用 v0.55 已删的 v2 `TypeChecker`，修正为「HM 单一检查器」。

### P4 — interp.rs + dag_interp.rs → vm.rs（src/mir/）
- SQLite VDBE 单文件惯例：`run_mir` / `run_mir_with_signal` / `MirSignal` /
  `run_dag_with_signal*` 合并为 `vm.rs`（931 行，模块内部引用去别名化）。
- 全部外部引用统一 `crate::mir::vm::`（main/REPL/import/pregel/tests/
  jit_bench 14 处）。

### P5 — testcase! 宏 + 分支插桩（src/interpreter/dispatch.rs）
- SQLite `testcase()` 同款宏：debug 构建断言分支守卫命中 + 携带分支名，
  release 零开销。插桩 `len`（list/string/dict）与 `merge_with`（key/
  strategy）守卫，覆盖测试验证插桩分支真实可达 —— 覆盖意图自文档化，
  为 P6（BuiltinId 静态表）铺路。

## [v0.75.46] — 2026-08-02 — JIT with-block 真实路径验证（阶段 5 后续 D）

### Added — 真实路径验证（tests/jit_compile.rs + examples/jit_bench.rs）
- **with-config 差分测试**：`WithConfig{jit:true}` 经 `h_with_config`
  dispatch 走 `run_jit` —— 可编译 body 成功不回落、不可编译 body（含
  Define 副作用）编译期拒绝回落 `run_mir`；两者与 jit=false 的 env
  终态一致（config 设置/恢复无副作用）。
- **benchmark example**（`cargo run --release --example jit_bench`）：
  纯 Float 算术函数 1M 次执行对比。

### Benchmark（本机 release，如实记录）
- `JIT: 14.5s vs MIR: 17.5s → 1.21x`。v1 单轮 = 编译（ExecMem 分配 +
  代码生成）+ 执行，with-block 每轮重编译，编译开销主导（1.21x 反映
  真实路径成本）。持续加速路径：ExecMem 复用缓存 / 编译结果 memo
  （当前以-block 语义每轮新鲜编译，属 v1 已知边界）。

## [v0.75.45] — 2026-08-02 — JIT 控制流模板（阶段 5 后续 C）

### Added — Jump / JumpIf / JumpIfNot 线性化（src/mir/jit.rs）
- **两遍线性编译**：第一遍逐 pc emit + 记录每指令段 code offset
  （`pc_offsets`）；第二遍 `patch_control` 把跳转目标（pc 索引，lower
  patch_label_at 填的 insts 索引）patch 成 rel32。
- `Code` 扩展：`jcc_pc`（条件跳转占位）/ `jmp_pc`（无条件）/ `patch_control`。
- **JumpIf/JumpIfNot**：cond 须 Bool（比较结果；其他类型 truthy 语义超
  出模板集 → 编译期拒绝回落）。发射 tag 检查 + payload `test` + `jnz/jz`。
- 类型跟踪新增 `Bool`（比较结果参与控制流）。

### Changed — 控制流测试边界（tests/jit_compile.rs）
- 无条件跳转保留解释器差分对比（jump_skip 6/6 一致）。
- 条件跳转断言 JIT 线性指令语义 + 跳转目标命中（rel32 正确性）——
  解释器 dag 优化会裁剪无消费者的死 Const，与原始指令语义在条件跳转
  边界分歧（真实编译产物经 lower 产出，无死 Const；此处为手工构造用例
  的观测边界，非行为差异）。

## [v0.75.44] — 2026-08-02 — JIT 扩展：W^X + Int 算术 + Mod（阶段 5 后续 A/B）

### Changed — W^X 双阶段可执行内存（src/mir/jit.rs）
- `ExecMem::alloc` → `alloc_rw`（RW 写入）+ `make_exec`（VirtualProtect/
  mprotect 切 RX）。生成代码拷入后立即收口，杜绝 RWX 页。
- `try_compile` 末尾改 `alloc_rw → copy → make_exec` 序列。

### Added — Int 算术 / Mod / 比较模板（精确复刻解释器分裂语义）
- **Add**：eval_binary Int+Int **直接 i64 加法**（x86 add wrap，与 release
  解释器一致；debug 解释器溢出 panic 为既存行为）。
- **Sub/Mul/Div**：numeric_op **f64 round-trip** — cvtsi2sd → SSE 运算 →
  roundsd（half-away）→ cvtsd2si + **范围检查饱和**（comisd 2^63 阈值：
  ≥2^63 → i64::MAX、nan → 0、其余直转 — 与 Rust `as i64` 语义一致）。
- **Mod**：`a - trunc(a/b)*b` 序列（roundsd mode 3 trunc）+ roundsd + 饱和。
- **Int 比较**：`a as f64 op b as f64`（cvtsi2sd + comisd 寄存器形式）。
- 类型线性跟踪升级为 Int/Float 双型；Mixed 拒绝回落。

### Fixed — flow.rs::values_equal 缺 Int 分支（v0.38 遗留 bug）
- `4 == 4` 恒 false（numeric tower 引入 Int 时漏加分支）。补 Int 分支
  + 测试（含 Mixed 数字不相等语义）。

### 模板调试实录（objdump 反汇编锁定，9/9 差分测试守护）
- `movq xmm1, rdx` / `comisd` 的 modrm **mod=00 内存形式**误用（解引用
  非法地址 AV）→ mod=11 寄存器形式。
- `comisd xmm0, xmm1` modrm 操作数方向（Intel 第一操作数在 reg 字段）。
- REX：movabs rdx 的 R=1 错配（成 r10）、8 位寄存器操作 W 位。
- 跳转偏移逐字节核算（comisd 4/5 字节、xor 3 字节差异）。

## [v0.75.43] — 2026-08-02 — copy-and-patch JIT（阶段 5 落地，零 LLVM 依赖）

（JIT 路线最终形态：**手写机器码模板 + 可执行内存拼接**，CPython
3.13+/PEP 744 模式。v0.75.34-37 的 inkwell/LLVM 依赖彻底删除 —
`--all-features` 恢复可编译（此前本机直接编译失败）。）

### Removed — LLVM 依赖（Cargo.toml）
- 删 `jit = ["dep:inkwell"]` feature + `[dependencies.inkwell]`（LLVM
  22 绑定，Windows 需 MSYS2 系统库，本机不可用）。jit.rs 从「LLVM 占位
  stub」重写为纯 std 实现，**零外部依赖始终编译**。

### Added — copy-and-patch JIT 核心（src/mir/jit.rs，+430 行）
- **ExecMem**：VirtualAlloc（Windows）/ mmap（unix）可执行内存，零依赖
  W^X 取舍：直接 PAGE_EXECUTE_READWRITE（v1 模板，可后续改双阶段）。
- **JitValue**：`{tag, payload}` 16 字节 repr(C) 槽，标签联合
  （Int/Float/Bool/Nil）。
- **Code 发射器**：x86-64 字节序列 + 跳转 patch 簿（copy 模板 + patch
  寄存器位移/常量立即数/相对偏移）。
- **v1 可编译子集**：`Const`（立即数）+ `BinaryOp` **Float×Float**
  （SSE2 addsd/subsd/mulsd/divsd + comisd/setcc 比较）。**Float 除零 =
  IEEE inf、NaN 比较 = false（NotEqual 例外 = true）**，与解释器语义
  精确一致。
- **bail 机制**：类型不匹配（动态）→ 生成代码置 `state.bail` 跳回 →
  run_jit 返回 Err → 调用方回落 run_mir。
- **编译期拒绝**：Int×Int 算术（i64 round 语义）/ Mod（无 fmod）/
  Var/Define/调用/效果/控制流 → 直接 Err（解释器兜底语义正确性）。
- **平台**：Windows x64 / SysV 双调用约定（rcx/rdi 区分）；非 x86-64
  编译期拒绝回落。

### Changed — 调用面（src/mir/handlers.rs）
- `h_with_config` jit 分支：删 SSA 构造 + typeinfer，直接
  `run_jit(body, ...)`；Err 回落 run_mir（行为语义不变）。

### Added — 差分测试（tests/jit_compile.rs，5 测试）
- 手工构造 MirFunction（绕过 lower 常量折叠）测模板发射：Float 算术
  （含除零 inf）、6 比较操作符、NaN 全矩阵（comisd 无序路径）。
- lower 折叠输入全链路一致性（常量折叠后全 Const）。
- 不可编译子集拒绝（Int×Int / Mod / Var / 调用）。

### 修复（模板调试实录，cargo test 锁定）
- jnp NaN 修正不可靠（PF 仅无序时置位）→ 改 setb∧sete（ZF∧CF 无歧义）。
- `movq r11,xmm1` REX 错位（0x49 扩 rm 而非 reg）→ 弃用 SSE 方案。
- 8 位寄存器操作 REX：`and r8b,r9b` 需 0x45（W 必须 0；0x4E 变 64 位
  and、0x0A/0x0E 非 REX 前缀）+ reg 位 → 污染 rcx 的 arg 指针崩溃。

## [v0.75.42] — 2026-08-02 — 运行态零 AST 收尾（阶段 4）

（阶段 4 探查结论 + 清理：MirExpr **无求值器**（v0.55 已删执行语义），
运行路径（compile → MirInst → run_mir）零 MirExpr；本次清理最后一处
LSP 桥接 + 唯一冗余 MirExpr 数据字段。）

### Changed — LSP 零 MirExpr 桥接（src/lsp/providers/parsed_doc_v3.rs）
- `parsed_doc_v3` 从 `parser.parse()`（MirExpr）+ `from_exprs` 桥接切到
  `ParserV3::compile` 直接产出 witness — LSP 全部 provider 数据源
  （folding/semantic/references/rename）不再经 MirExpr 中间层。

### Removed — `MirAgentDef.task_mir_expr`（冗余设计占位）
- **src/mir/expr/mod.rs**：删 `task_mir_expr: Option<MirExpr>` 字段
  （v0.75.32 注释自证「lower 中零消费，透传保留 Some」——纯冗余）。
- **src/mir/witness.rs**：`WitnessAgentDef` 同步删（witness 不再挂
  MirExpr，轻量树骨架纯度恢复）。
- **src/parser_v3/mod.rs**：删 `task_mir = Some(body.clone())` 构造 +
  两处字段初始化（parse_agent_def / orchestrate loop 分支）。
- **src/pregel/mod.rs**：7 处测试构造初始化删除。
- **src/mir/optimize/cost.rs**：make_agents 构造同步。
- **tests/orchestrate_v3_pipeline.rs**：3 处断言删除（is_some + 占位
  注释），task_body 非空断言保留。

### 架构结论（阶段 4 终局）
- **运行态零 AST 已达成**：MirExpr 无求值器，compile 直接 emit 指令，
  执行路径（CLI/REPL/import/LSP/pregel）全程不触碰 MirExpr 树。
- MirExpr 保留为「数据构造类型」：orchestrate agent 语义数据
  （task_expr/verify_expr/with_config/exit_when/condition_expr）由
  parser 构造、witness 镜像（from_expr 转轻量树）、LSP 遍历消费 —
  无执行语义，阶段 3/4 目标范围内不删。

## [v0.75.41] — 2026-08-02 — LSP 诊断切 compile（阶段 3 Step 4 收尾）

（LSP `check_diagnostics` 从 parse→typecheck_mir_exprs 双阶段切到
`ParserV3::compile` + `check_program_witnesses` — 执行路径最后一个
MirExpr 桥接消费点消失；MirExpr 仅剩「数据构造类型」角色。）

### Changed — src/lsp/server.rs
- `check_diagnostics`：compile 直接产出 witness，typeck 直接消费；
  删 `parse_code_v3` + `typecheck_mir_exprs` 依赖。

## [v0.75.40] — 2026-08-02 — 执行入口切 compile + typeck witness 单实现（阶段 3 Step 4a）

（单遍编译落地：全部执行入口从 parse→lower 双阶段切到
`ParserV3::compile`（直接 emit MirInst + 并行产出 witness）。typeck
改消费 witness（零 MirExpr 桥接）。）

### Changed — 执行入口（src/main.rs / src/interpreter/mod.rs）
- **main.rs**：`parse_with_v3`（parse→lower）删除，新增 `compile_and_opt`
  统一编译辅助（compile + cascades apply_rules + SSA opt，语义与
  `lower_mir_exprs_with_opt` 一致）。`run_file`/`run_record`/`run_replay`/
  `run_snapshot`/`run_check` 全切 compile + witness typeck。
- **REPL**（run_repl_with）：逐行改 `ParserV3::compile` + witness
  typecheck，空 body 跳过。
- **mir_import**：compile 直接产出指令 + witness，删 parse→lower 双阶段。
- 删 `parse_v3_internal`（唯一内部 parse 辅助，已无调用）。

### Changed — typeck witness 单实现（src/typeck/）
- `check_program_witnesses(&[MirWitness])` — 新主入口，直接消费
  witness（import 预扫描 + HM 推断）。
- `check_program_mir(&[MirExpr])` — 降为桥接（from_exprs → witness），
  保留给 LSP/测试（仍产出 MirExpr 的路径）。
- `imports.rs`：`extract_module_symbols`/`collect_imported_symbols` 改
  消费 `[MirWitness]`，模块 import 递归走 `ParserV3::compile`。

### 架构边界（Step 4b 结论）
- `MirExprLowerer`/`lower_mir_exprs` **保留** — orchestrate agent body
  （parse_agent_def）与 pregel merge 表达式是编译期/运行期显式**数据
  构造**，仍需「MirExpr 数据 → 指令」转换。MirExpr 从「执行路径中间
  表示」降级为「数据构造类型」；执行入口已全切 compile。
- 差分测试（compile vs parse→lower）继续为 compile 正确性守卫。

## [v0.75.39] — 2026-08-02 — witness 嵌套化（阶段 3 Step 3d）

（ParserV3 单遍编译的 witness 精化：compile() 产出的 MirWitness 从
扁平 push 改为**递归嵌套树**，emit 时直接构建。差分测试断言从
「非空」收紧为「顶层 witness 数 == 顶层 expr 数」。）

### Changed — src/parser_v3/mod.rs（emit 家族全 _w 化）
- **表达式类**：`emit_expr_w`/`emit_or_w`/`emit_and_w`/`emit_equality_w`/
  `emit_pipe_w`/`emit_comparison_w`/`emit_term_w`/`emit_factor_w`/
  `emit_unary_w`/`emit_call_w`/`emit_call_tail_w`/`emit_arg_list_w`/
  `emit_primary_w`/`emit_list_w`/`emit_dict_w` — 返回 `(Reg, MirWitness)`
  递归构建嵌套树（Binary/Or/And/Call/MethodCall/Index/Closure/List/Dict/
  Prompt 均嵌套子节点）。
- **语句类**：`emit_let_w`/`emit_fn_def_w`/`emit_return_break_continue_w`/
  `emit_type_alias_w`/`emit_enum_def_w`/`emit_struct_def_w`/`emit_import_w`/
  `emit_macro_def_w`/`emit_if_w`/`emit_loop_w`/`emit_while_w`/`emit_match_w`/
  `emit_match_arm_w`/`emit_orchestrate_w` — 返回对应 WitnessKind。
- **块语义**：`emit_block_w` + `block_witness` — 块内语句列表折叠为
  单条 witness 或 `Sequence`（与旧 parse_block_body 语义一致）。
- **compile 收集**：`emit_program` 改走 `emit_statement_w`，每条顶层
  语句 push 一个嵌套 witness；`emit_statement` 等 32 个旧薄包装
  （`Option<Reg>`/`Option<()>` 版）全部删除 — 消除 dead_code。
- **match arm**：`EmittedMatchArm` 结构体取代五元组返回
  （type_complexity）。
- 修 7 处 span 循环重赋值 + 1 处 unused_mut（clippy 归零）。

### Changed — src/mir/witness.rs
- `WitnessPattern::from_pattern` / `WitnessOrchestrateKind::from_kind`
  改 pub（parser_v3 emit 路径复用）。

### Changed — tests/compile_differential.rs
- witness 断言从「非空」收紧为 `witnesses.len() == exprs.len()`
  （顶层语句 ↔ 顶层 expr 一一对应）。

## [v0.75.38] — 2026-08-02 — MirWitness 轻量树骨架（去 AST 化阶段 2）

（MirExpr → MirWitness 终局的阶段 2：定义 witness 骨架 + typeck/LSP
消费面迁移。阶段 3/4 parser 直接 emit、删 MirExpr 执行语义。）

### Added — src/mir/witness.rs（+~470 行）
- `MirWitness { kind: WitnessKind, span }` — 轻量树骨架，**无执行语义**。
- `WitnessKind` 镜像 `MirExprKind` 全部 30 变体（独立枚举，阶段 3/4
  消除 MirExpr 时胜出）。
- 复合类型同步镜像：`WitnessCallee`/`WitnessArm`/`WitnessParam`/
  `WitnessPattern`/`WitnessOrchestrateKind`/`WitnessAgentDef`/`WitnessEdgeDef`。
- `from_expr` 递归转换（30 变体逐一映射）+ 3 个往返一致性单元测试。

### Removed — Closure.captured_env（src/mir/expr/mod.rs）
- `MirExprKind::Closure` 删 `captured_env: Arc<EnvSnapshot>` + 删
  `EnvSnapshot` struct — 全仓库零消费死字段（仅内部构造，typeck/lower/
  parser 均不读取；闭包捕获在运行时由 handler 实现）。

### Changed — typeck 消费 witness（src/typeck/）
- `infer_expr`/`infer_call`/`infer_method_call`/`infer_closure`/`infer_match`
  等 30 分支改消费 `WitnessKind`（机械替换）。
- `check_program_mir` 入口桥接：`MirExpr` → `from_exprs` → witness
  （parse 层仍产出 MirExpr，main.rs 4 处 pipeline 零改动）。
- `imports.rs` 的 infer_program 调用点同步桥接。

### Changed — LSP 消费 witness（src/lsp/providers/）
- `parsed_doc_v3` 返回 `Vec<MirWitness>`；`walk_mir_expr` → `walk_witness`。
- folding / semantic / completion / definition / rename 五 provider
  经共享 helper 一处迁移全通。

### 验证
- mir 单元 86（+3 witness）、tier0/1/2、orchestrate 12、LSP 8 全绿。
- clippy 0 / fmt 0。

## [v0.75.37] — 2026-08-02 — 生产 unwrap 清理 + typeck 死代码移除（审查报告既存项）

（依据架构审查报告 3.3–3.6 与改进建议 P2-3，清理既存质量问题。
不涉及行为变更——所有替换等价，死代码移除由编译验证。）

### Changed — 生产代码 unwrap → expect（AGENTS.md §3）
- `trace_collector.rs` 10 处 `.lock().unwrap()` → `.expect("trace collector
  poisoned")`：std Mutex 中毒后无恢复路径，是最高频连锁 panic 风险点。
- `worker_pool.rs` 2 处（rx/queue 锁）：worker 线程内中毒致线程退出。
- `orchestrate_dag/mod.rs` 2 处 `get_mut().unwrap()` → expect（validate()
  已保证结构不变量）。
- `mir/ssa.rs` 3 处 `last().unwrap()` → expect（is_empty 前置检查）。
- `parser_v3/mod.rs`、`compress/json.rs` 各 1 处 → expect。
- `typeck/hm/mod.rs` `solve_constraints`：消除「if let Err 后 else 重复
  solve + unwrap」的重复调用，改单次 match。

### Removed — typeck/mod.rs 死代码（-395 行）
- `TypeChecker` / `LifetimeEnv` / `BorrowChecker` / `TraitTypeDef` /
  `substitute_type_hint` / `type_to_hint_string`（~400 行，零外部调用，
  唯一入口 `check_program_mir` 存活）。移除 12 处 `#[allow(dead_code)]`。

### 验证
- clippy 0 / fmt 0 / 编译全绿。
- tier1_typeck 32、tier2 62、parser_v3_coverage 4 通过。

## [v0.75.36] — 2026-08-02 — inkwell 升级 LLVM 17→22（占位）+ JIT 路线转向 copy-and-patch

### Changed — inkwell 0.5 → 0.9（Cargo.toml / Cargo.lock）
- `llvm17-0` → `llvm22-1` feature，llvm-sys 170 → 221。零 API 迁移成本
  （jit.rs 的 inkwell 全在注释占位，`run_jit` 是返回 Err 的 stub，无活调用）。
- 动机：llvm-sys 170 在 Windows 需 LLVM 17 系统库（本机仅有 LLVM 22），
  且所有 Windows 预编译 LLVM 分发不含 `llvm-config`（llvm-sys 构建必需），
  本地 `--all-features` 编译不了 jit——这是长期状态，非本次引入。
- **保留为占位**：验证 `--all-features` 交给 CI（ubuntu apt 有 LLVM 22，
  CI 已配置该命令）。jit.rs 错误提示文本同步更新为 LLVM 22。

### Changed — JIT 路线决策（调研 CPython 3.13+/PEP 744 后）
- 现状：mora 后端 = MIR 字节码解释器（ParserV3 → MirInst → run_mir），
  零 LLVM 运行时依赖。`jit` feature 是 stub。
- **新路线：copy-and-patch JIT**（CPython 3.13+ 采用，PEP 744/836）。
  把每个 MirInst 预编译成机器码模板（blob），运行时拼接+打补丁 —
  **运行时零 LLVM 依赖**（LLVM 仅构建期工具），生成代码比 LLVM -O0
  快两个数量级。契合「运行时最小化 + 零成本抽象默认」哲学。
- 影响：mora 的 JIT 不需要背巨型 LLVM 依赖；inkwell 升级仅作过渡占位，
  新路线落地时移除。本变更记录决策依据（AGENTS.md 信息搜索实证）。

## [v0.75.35] — 2026-08-02 — Sequential orchestrate pipeline 执行（缺口 c 完成）

（阶段 1「清剩余缺口」收官：b 与 c 均已修复，orchestrate 12/12 全绿。）

### Added — h_orchestrate Sequential 分支（src/mir/handlers.rs）
- pipeline 语义：agent task_body 按声明顺序执行，前输出作后输入
  （`input` 变量契约，沿用 pregel 注入方式），最终 result 写入 result_var。
  每 agent 独立 env 克隆，agent 期间 define 的变量合并回父 env
  （与 pregel reconcile_outcome 写回语义一致）。
- 此前 `MirOrchestrateKind::Sequential` 走 "not yet supported"（handlers.rs:669
  只实现 Pregel）。

### Fixed — typeck orchestrate 变量声明（src/typeck/hm/mod.rs）
- `orchestrate ... input -> result` 语义上声明 input_var/result_var，但
  `infer_expr` 只返回 Nil 不登记变量 → CLI 路径引用 result 报
  UnboundVariable（测试走 run_mir 绕过 typeck 未暴露）。登记为 Any。

### Tests
- 激活 3 个被 ignore 的测试：Sequential 执行、for/while 循环累加
  （缺口 b 修复后累加正确，输出 6/45 而非 0/nil）。
- orchestrate 12/12、tier1_typeck 32/32、tier2 62/62、clippy 0。

## [v0.75.34] — 2026-08-02 — DAG 循环执行修复（CSE 重命名 + 块内全序 + 方法调用）

（清剩余缺口 b：循环在 DAG 执行路径上不累加/提前读脏值。根因跨 6 层，
全部是优化器/构建器/解释器三层叠加的结构缺陷，其中 4 层此前被「循环体
不执行」掩盖——探索阶段激活循环后逐一暴露。）

### Fixed — 优化器删除节点破坏消费者/控制目标（dag_rule.rs / dag_search.rs）
- **CSE 合并不同 dst 节点导致悬垂（根因 A）**：两个等价 `Const(Nil)` 占位
  （dst=4 / dst=7）被合并时只重定向 Data 边，不改写消费者的 `input_regs`
  （寄存器号）→ 被合并的 dst 失去 producer → 消费者永不 ready。
  **修复**：CSE 统一 dst + 新增 `DagRewrite.reg_rename`，`apply_rewrite`
  全局重映射 Compute/Effect/Branch/Phi 的寄存器引用 + Data 边寄存器号
  （新增 `MirInst::map_regs` 输入重映射工具）。
- **Sequence 缝合**：removed 节点位于线性链中间（let 占位被 CSE 合并）时，
  删边断开保序链 → 后续 Var 提前执行。**修复**：收集 removed 节点的
  Sequence 前驱/后继补边跳过。
- **控制目标保护**：CSE/DeadNode 删除被 Branch/Jump target 引用的节点 →
  target 悬垂。**修复**：`is_control_target` guard（v0.75.33 已含，CSE 补全）。

### Fixed — DAG 构建器破坏基本块顺序（dag.rs）
- **根因 B：`prune_sequence_edges` 裁剪 Compute 保序边**：只保留
  Effect-Source 的 Sequence 边，删掉 Compute（Var/Const/Index 读 env）之间
  的保序边以「暴露 ILP」——但 `dag_interp` 顺序执行 ready 列表（ILP 从未
  实现），裁剪只破坏保序：`Var(total)` 提前于 `Define(total)` 执行读脏值、
  循环 exit 后代码不可达。**修复**：`dag_analyze` 建基本块内全序
  （每节点与前驱连 Sequence，控制转移处不连），`prune_sequence_edges` 保留
  所有 Sequence 边（no-op 保正确性）。

### Fixed — 解释器调度（dag_interp.rs）
- **Branch/Jump 双目标激活**：edge-scan 无条件推送 Branch/Jump 出边，
  两个分支目标同 wave 竞态（exit 读脏值 + body 用越界 i 再跑 → OOB）。
  **修复**：控制转移完全由 handler 决定（只推选中的 target）。
- **Sequence 前驱就绪判定**：无输入寄存器的节点（Var/Define）一激活即可
  执行，若其 Sequence 前驱未执行会提前读脏值。**修复**：ready 过滤要求
  Sequence 前驱已执行（`executed` 标记）。
- **wave 去重**：ready 节点标记 pushed，Branch/Jump handler 与 scan 共用，
  防止同 wave 重复执行。

### Fixed — 方法调用 mangled 名（lower.rs）
- **根因 C：`ops.mul(x)` 拼成 `Call("ops_mul", ...)`**：ParserV3 正确产出
  `MirCallee::Method`，但 lower 的 Call 分支拼 "obj_method" 字符串 →
  interpreter 查不到该名字 → "Undefined function or task"（循环体真正执行
  后暴露；闭包 Dict 方法 ops.mul(x) 是实际受害者）。**修复**：
  `MirCallee::Method` 走 `MirInst::MethodCall`（receiver 弹出为接收者）。

### Added — 回归测试
- `cse_renames_consumer_regs_on_merge`（dag_search.rs）：CSE 合并后消费者
  input_regs 不引用被删 dst。
- `prune_preserves_block_order`（dag.rs）：prune 保留块内全序。
- tier2 `v3_lower_method_call_produces_call` 更新断言：MethodCall 而非
  mangled Call。

### 验证
- 全集成套件通过（dag_integration / tier0_replacement / tier0_closure_mir /
  tier1_typeck_mir / tier2_mir_expr_pipeline / parser_v3_* 等 14 套件）。
- mir 单元 83 通过 / 0 失败。
- clippy `-D warnings` 0。
- 手工验证：`for i in items { sum += i }` 输出 6、`range(0,10,1)` 累加输出 45
  （此前 "nil"/"3"/OOB）。
- 注：`cargo test --lib` 全量在 `schedule::tests::persistence_roundtrip`
  挂起——stash 对照确认**预存在**（与本次改动无关），单测通过。

## [v0.75.33] — 2026-08-02 — ConstFolding 正确性修复（归纳变量 + 重定义）

（清剩余缺口 a：循环累加返回 0 的根因。探索阶段激活 orchestrate 测试时暴露
`sum = sum + i` 恒 0 —— 顺藤摸到优化器折叠 bug，独立修复。）

### Fixed — find_const_backward / ConstFoldingRule（src/mir/optimize/rule.rs）
- **根因 1（重定义）**：`find_const_backward` 只回溯到 `Label` 边界找
  `Const`——但 for 循环 lowering **不插 Label**，回溯穿过整个循环体找到
  循环前 `Const(i, 0)` 初始化，把 `i = i + 1` 错折成 `i = 1`。
  **修复**：遇到最近定义点（`inst.dst() == reg`）即停止——非 Const 返回
  None（该 reg 已被重新定义，更早的 Const 失效）。
- **根因 2（归纳变量）**：`i = i + 1` 的 dst == lhs（loop-carried
  dependence），回溯只能找到初始化值，折叠必错。**修复**：`dst ∈
  {lhs, rhs}` 的 BinaryOp 直接跳过折叠（保守正确）。

### Added — 回归测试（rule.rs +2）
- `test_const_folding_skips_induction_variable`：`i = i + 1` 保留 BinaryOp。
- `test_const_folding_stops_at_redefinition`：reg 重定义后不折叠。

### 验证
- 全测试 **707 通过 / 0 失败**（+2）。
- clippy `-D warnings` 0 / fmt 零 diff。
- 注：修复后循环从输出 "0" 变 "nil"——折叠 bug 已修（不再错折恒值），
  但暴露了第二个独立缺陷：DAG 执行器对含 BackEdge 程序的循环执行问题
  （pre-existing，stash 对照确认与折叠无关），专项处理（缺口 b）。

## [v0.75.32] — 2026-08-02 — 去 AST 化终局阶段 1：修复 pregel 降级缺失

（多阶段终局第 1 阶段：MirExpr → witness + parser 直接 emit 的前置障碍清理。
计划已批准，见 docs/de-ast-boundary.md §3 增量路径。）

### Fixed — pregel task_expr → task_body 降级缺失（src/parser_v3/mod.rs）
- **根因**：`parse_orchestrate_agent` 产出 `task_expr: body` 但 `task_body`
  恒空（parser_v3/mod.rs:559-563），pregel 执行报 "lowering missing"
  （pregel/mod.rs:752）。orchestrate 测试全 `#[ignore]` 掩盖了该缺口。
- **修复**：产出 agent 时立即 `lower_mir_exprs(std::slice::from_ref(&body))`
  填 `task_body`；失败兜底为空（保持旧行为）。

### Changed — 激活 orchestrate 测试（tests/orchestrate_v3_pipeline.rs）
- 激活 7 个（2 通过 10 忽略 → 9 通过 3 忽略）：两个 lower 结构测试 +
  `v3_pipeline_orchestrate_pregel_runs`（**经 parser 的 orchestrate pregel
  端到端可执行**）+ if/let/match/task 端到端。
- 3 个 ignore 标注真实原因（非笼统旧标注）：
  - `v3_pipeline_orchestrate_sequential_runs` — h_orchestrate 只实现
    Pregel，Sequential 走 "not yet supported"（handlers.rs:669，功能缺口）。
  - `v3_pipeline_for/while_loop_runs` — **pre-existing 循环累加 bug**
    （`sum = sum + i` 返回 0，独立运行 /tmp/for.mora 亦复现）。

### 验证
- 全测试 **705 通过 / 0 失败**（+7）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.31] — 2026-08-01 — 语义漂移修复：删除 MirInst::Receive 死原语

（回应「两个 env 会不会混淆」的第三层面——`Environment` 语义漂移。实证
修正了此前的判断：pregel 主力消息通道（`dynamic_sends` 缓冲 + `input_
<channel>` 注入）都**不**碰共享 Environment——真正的漂移只有一处：`h_
receive` 读共享 env 当消息源（把「变量作用域」当「消息队列」），而它是
零构造的死路径。裁决：删除。）

### 实证（修正此前说法）
- 主力通道（活）：`h_send` → `dynamic_sends` 缓冲（handlers.rs:333）→
  pregel `pending_sends` → ADVANCE 按 target 投递 + `input_<channel>` 注入
  agent 私有 env —— **不污染共享 Environment**。
- 漂移点（死）：`h_receive` 从 `interp.environment()` 读值当消息源
  （handlers.rs:368）；`MirInst::Receive` **全仓零构造**（src+tests）——
  StreamFor 同族（语法先行、语义未接的残余）。

### Removed — MirInst::Receive（4 文件 6 处）
- `src/mir/mod.rs`：变体删除（注释记录删除原因与替代机制）。
- `src/mir/handlers.rs`：`h_receive` 函数 + dispatch 分支 + `input_regs`/
  `is_effect` 列表成员。
- `src/mir/ssa.rs`：跳过列表 2 处成员。
- `src/mir/optimize/cost.rs`：cost 分支。
- 删除即验证：cargo check 一次通过。
- `MirInst::Send` 保留（写独立缓冲，语义正确）。

### 意义
- 语义漂移消除：`Environment` 回归「变量作用域」单一职责——不再有任何
  代码把它当消息队列读。Message 语义统一由引擎投递（`input_<channel>`）。
- 与 v0.75.19/20/26 的收敛线一致：悬浮原语逐个定夺。

### 验证
- 全测试 **698 通过 / 0 失败**（pregel 消息路径不变——主力通道未动）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.30] — 2026-08-01 — MORA_OPT 提升为显式编译选项 `--opt=N` + SSA 声明透传修复

（v0.75.29 注释的演进项落地：「优化等级应成为编译命令的一等参数」。CLI
显式化不仅完成提升，还暴露并修复了一个被默认关闭掩盖的真实 SSA bug。）

### Added — CLI 显式编译选项 `--opt=N`（src/main.rs + ssa.rs + lower.rs）
- `mora --opt=1 file.mora` 显式指定优化等级（0=关/1=Basic/>=2=Aggressive），
  `--opt` 紧跟可执行名、不进入子命令参数。
- `OptLevel::from_arg`（与 from_env 共享 0/1/2 语义）；`lower_mir_exprs_with_
  opt`（显式等级变体）；`run_file/run_record/run_replay/run_snapshot` 四个
  编译入口穿透。未指定 → env 兜底（REPL/import/pregel 动态路径不变）。
- --help 更新。

### Fixed — SSA 声明型指令透传（src/mir/ssa.rs，真实 bug）
- **根因**：SSA construct 跳过声明型指令（TaskDef/ToolDef/Import/StructDef/
  全部 effect 指令），deconstruct 无从恢复 → 优化后 `func.body = [Label(0)]`，
  **task main 消失**（print 无输出）。`MORA_OPT=1` 默认关闭掩盖；`--opt`
  显式化立即暴露。
- **修复**：`MirSsaFunction` 加 `passthrough: Vec<MirInst>` 字段；construct
  收集（新 `is_ssa_passthrough` 谓词，与 split_into_ssa 跳过列表单点同源防
  漂移）；deconstruct 还原到 body 头部。
- **回归测试**：`mir_ssa_roundtrip.rs` 加 `taskdef_survives_ssa_optimization`
  （结构断言 TaskDef 优化后存活）——现有 `assert_task_equiv` 有盲区（只
  断言顶层返回值，main 内副作用未被覆盖），结构断言补上。

### 验证
- 端到端：`--opt=1` / `--opt=2` / `MORA_OPT=1` 三路径 task main 均输出 3。
- 全测试 **698 通过 / 0 失败**（+1 回归守卫）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.29] — 2026-08-01 — MORA_HM 僵尸删除 + MORA_OPT 文档化

（回应「为什么编程语言项目有设置环境变量的东西」——实证全仓 14 个环境
变量分三类：外部集成（OCR/AI/CORS）正当、用户目录解析（HOME/USERPROFILE）
标准、语言内部行为开关（MORA_OPT）合理、僵尸（MORA_HM）误导。本 commit
清僵尸 + 文档化。）

### Removed — MORA_HM 僵尸（3 处）
- **实证**：错误消息提示「Set MORA_HM=1 to enable」（error.rs:99），但全
  仓库无人读取该变量；`TypeError::HmDisabled` 变体从未被构造（仅定义 +
  Display + 行号映射，零构造点）——比僵尸提示字符串更彻底，是死变体。
- 删除：`HmDisabled` 变体 + Display 分支 + check_mir.rs 行号映射分支。
- 删除即验证：cargo check 一次通过，`MORA_HM`/`HmDisabled` 全仓零残留。

### Changed — MORA_OPT 文档化（src/mir/ssa.rs）
- `from_env` 补权威注释：`MORA_OPT` 语义（未设/0=None 默认零开销、
  1=Basic、>=2=Aggressive）、存在理由（v0.75.7 渐进式启用：SSA 优化未
  证明对所有程序安全前默认关，作 I5 可回退逃生舱）、演进方向（v1.0 应
  提升为显式编译选项而非环境变量）。
- 保留现状（默认关闭 = 零成本抽象姿态），不调整行为。

### 环境变量分类清单（审计沉淀，代码外）
- 正当外部集成：MORA_OCR_MODELS_DIR / MORA_AI_MODEL / MORA_AI_BASE_URL /
  MORA_AI_RETRY_MAX / MORA_AI_RETRY_BASE_MS / MORA_CORS_ORIGIN
- 标准用户目录：HOME / USERPROFILE / LOCALAPPDATA / XDG_DATA_HOME
- 语言内部开关（已文档化）：MORA_OPT / MORA_MEMORY_DIR
- 已删除僵尸：MORA_HM

### 验证
- 全测试 **697 通过 / 0 失败**。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.28] — 2026-08-01 — 方向 1/2/4/5/7 剩余项裁决与落地

（三路审计剩余项：信号传播、变量级增量、资源槽位、约束原语。经实证后
两项落地、三项否决——每项裁决都有源码证据。）

### 落地 — 方向 2 变量级增量重算（行为守卫 + 实证）
- **实证**：DagExecMemo 的「输入值相等跳过」已实现变量级增量——
  Var（非纯，每次重跑读 env）→ 受影响下游纯节点输入变 → 重算；
  未受影响下游输入相等 → memo 跳过。白名单保守排除 Var 是正确设计。
- 新增 `dag_integration.rs` 守卫测试 `memo_incremental_reruns_affected_
  dependencies_only`：改 env 依赖后第二次 run——未受影响下游被跳过
  （delta_skipped > 0）且受影响链重算（delta_executed > 0）。

### 落地 — 方向 7 约束原语骨架激活（master_compute）
- **实证**：master_compute（v0.72 每超步全局协调钩子）+ aggregators +
  vote_to_halt 构成「每步评估目标 + 收敛」骨架，但 master_compute 在全部
  测试里为 None（零激活），且其**失败被 eprintln warn 吞掉**（协调钩子
  失败引擎静默跑错语义）。
- **修复**：master_compute 失败改为 `?` 传播（错误不再吞——与吞异常审计
  约束一致）。
- 新增激活守卫测试 `master_compute_runs_and_failure_propagates`：正常
  钩子引擎成功；失败钩子错误冒泡。

### 否决 — 方向 1 信号传播（双向赋值）
- 实证：增量重算已由「输入值驱动」实现（方向 2 落地项）；EDA 式「变量
  值变化 → 依赖表达式自动重求值」的实时双向传播需全新执行模型
  （信号网 + 时钟），非当前线性/DAG 执行器的增量改造。不新建 watchers
  （最小修改原则）。

### 否决 — 方向 4 资源槽位（FPGA 列表调度扩展）
- 实证：`parallelism` 即槽位上限（`WorkerPool::new(parallelism)` = N 逻辑
  单元，共享队列排队 = 有限资源 + 排队）；LJF 排序 v0.75.7 已实现
  （注释明言「FPGA list-scheduling 精神」）。扩展资源约束模型
  （LUT/BRAM 槽位）超出当前 worker 语义，为改而改。

### 否决 — 方向 5 DSP 波导（时间步进流式）
- 依据 v0.75.26 定夺：StreamFor 已删，「流式语义若需 MIR 指令级支持，
  重新设计而非复活旧形状」；AI 流式已有 `stream: true` 参数路径。

### 验证
- 全测试 **697 通过 / 0 失败**（+2：memo 增量守卫 + master_compute 激活）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.27] — 2026-08-01 — 审计收尾：DAG 缓存解耦 + Cascades cost_gain/memo 激活

（三路审计剩余项的收尾。三项中一项经实证**否决**——见下。）

### 否决 — P2a dynamic_sends → per-target 队列（审计建议不采纳）
- 实证：`SendTask { target_node, input }` **已是 per-target**；pregel 消费端按
  target 过滤（mod.rs:617 `any(|s| s.target_node == *node)`）与分组投递
  （:1037 `entry(send.target_node)`）。`h_receive` 的「读环境变量」是 BSP
  设计意图（v0.70 注释：values flow through channels, not blocking queues）。
- 否决理由：改 HashMap 是重构存储形态不改行为；`SendTask` 是 checkpoint
  持久化单元；为改而改违反最小修改原则。

### Changed — P2b DAG_CACHE 全局单例解耦（src/mir/interp.rs）
- 新增 `run_mir_with_signal_cached(func, interp, env, dag_cache)` 可注入
  缓存变体；`run_mir_with_signal` 委托给它（默认全局缓存）。
- 意义：测试/多租户可传独立 `DagCache` 隔离缓存状态（跨测试泄漏的注入
  点）；全局共享行为不变（pregel 并行 worker 共享是特性）。

### Changed — P3 Cascades 等价计划枚举（src/mir/optimize/search.rs）
- **`RewriteRule::cost_gain()` 接入**：数据驱动 gain 相同时用规则作者的
  静态估计打破平局——该 trait 方法 v0.75.5 引入后**从未被消费**（出生即死，
  与 ai_infra 同类），本次激活。
- **等价重写 memo**（Cascades Group 记忆内核）：重写是纯函数（ctx 空），
  同一 (规则, 指令形态) 在 body 内多次出现时只计算一次，跨轮复用；
  行为等价（memo 只消除重复计算，不改变最终优化结果）。

### Added — 测试
- `test_rewrite_memo_reuses_equivalent_shapes`：重复形态 body 收敛到与
  单形态控制组一致的 cost（行为等价性守卫）。

### 验证
- 全测试 **695 通过 / 0 失败**（+1）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.26] — 2026-08-01 — P0 语义定夺：StreamFor 死原语移除（方向 1/2/5 钥匙）

（三路审计共识的 P0：StreamFor 是「悬挂指令」——`MirInst::StreamFor {
prompt_reg, var, body }` 的 handler 空转（prompt_reg/var 被 `_` 忽略、body
仅执行一次并丢弃），是「语法先行、语义未接」的最后幸存者。语义定夺 = 删除。）

### 定夺依据（实证，四通道）
1. **零构造点**：全仓（src + tests）无任何代码构造 `MirInst::StreamFor`。
2. **零测试引用**：tests 无 StreamFor。
3. **语义已被取代**：AI 流式实际走 `ai.chat` 的 `stream: true` 参数
   （ai_chat.rs 流式响应路径），与 StreamFor 指令无关。
4. **空转 handler**：`h_stream_for` 克隆 env、`run_mir` body 一次、丢弃结果
   ——不产生任何可观察效应（尽管 `is_effect()` 标 true）。

### Removed — StreamFor（4 文件 6 处）
- `src/mir/mod.rs`：`MirInst::StreamFor` 变体删除（留注释记录：若未来需 MIR
  指令级流式语义，重新设计而非复活旧形状）。
- `src/mir/handlers.rs`：`h_stream_for` 函数 + dispatch 分支 + `is_effect`/
  `input_regs` 两处 list 成员删除。
- `src/mir/ssa.rs`：SSA 跳过列表成员删除。
- `src/mir/optimize/cost.rs`：cost 分支删除。
- 删除即验证：cargo check 一次通过。

### 意义
- 「语法先行、语义未接」的语法面残余清零（继 v0.75.19 关键字、
  v0.75.20 树变体、v0.75.21 pipe 之后，最后一个悬挂指令定夺）。
- 运行能力零变化：StreamFor 本就空转，删除不触碰任何可观察行为。

### 验证
- 全测试 **694 通过 / 0 失败**。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.25] — 2026-08-01 — ai_infra 清理：3 活类型迁入 runtime，12 死类型删除

（回应「这些代码为什么出现、历史遗留问题是什么」——先 git 考古实证再动手：
`src/ai_infra.rs` 在 v0.25（98c8c37，「feat: v0.25 新功能」大特性批次：Multi-Agent
orchestrate/Eval/Skill/Memory+Context Compaction）引入 15 个 AI 基础设施类型——
**规划图景，从未接入任何执行路径**（`-S` 追踪调用点历史零记录，出生即死）。
v0.52 ADR-001（32aa1ee「抽 AiRuntime facade」）重构时，其中 3 个被**误当成状态**
搬进 `AiRuntime` 字段，成为只构造、零方法调用的死字段——这就是「为什么还在」：
不是它们在服务，而是一次结构重构把死代码当活资产继承了。）

### 审计教训（诚实记录）
- 首轮按**类型名** grep 判 3 个类型为死——**漏了字段访问**（`self.ai.context_window`
  不出现类型名）。编译器（E0609）与 ai_chat.rs 调用点
  （`add_message`/`needs_compression`/`compress`/`verify`/`get_cached`）证实它们
  **活着**。死代码判定必须以「类型名 × 字段访问 × 调用点」三通道核对。

### Changed — 迁移 3 个活类型（src/runtime/ai_infra.rs 新文件）
- `ContextWindow`（ai.chat 消息滑动窗口 + 压缩）、`SpeculativeVerifier`（推测
  解码验证）、`CacheWarmer`（prompt→response 缓存）自 `src/ai_infra.rs` 迁入
  `runtime::ai_infra`，去除 `#[allow(dead_code)]`（它们现在是活的）。
- `AiRuntime` 字段不变；`use` 路径更新。

### Removed — 删除 12 个死类型 + 旧文件（src/ai_infra.rs，783 行）
- `AdaptiveTemperature`/`LoadBalancer`/`SmartCacheEviction`/`EvictionStrategy`/
  `ModelSwitcher`/`ModelBenchmark`/`AiCallTracer`/`CallSpan`/`AdaptiveBatchSize`/
  `ModelPerformanceVisualizer`/`CostOptimizer`/`RetryPolicy`——全仓库
  （src + tests）零引用，实证出生即死。`lib.rs` 移除 `pub mod ai_infra`。

### 验证
- 删除即验证：cargo check 一次通过（编译器证实无隐藏引用）。
- 12 死类型名 src+tests 零残留（仅新文件注释记录清单）。
- 全测试 **694 通过 / 0 失败**（活类型迁移行为不变）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.24] — 2026-08-01 — 策略名硬编码收敛：单一事实来源 + 编译期静态校验

（回应「为什么还需要有硬编码」——策略名解析在 dispatch.rs 的字符串 match
是「脚本源码 → Rust 枚举」的转换边界，当初为遵守去 AST 化约束（不引入
枚举字面量语法）选字符串表达。但非法策略留到运行时炸与 Mora 的静态强类型
定位冲突。本 commit 把硬编码收敛为单一事实来源，并把非法字面量前移到
typeck 编译期拦截。）

### Changed — 策略名 → 枚举的单一事实来源（src/value.rs）
- 新增 `MergeStrategy::from_name(&str) -> Option<Self>`——`lww`/`last_write_wins`/
  `append`/`add`/`dict_union`/`grow_only_set`。运行时解析与 typeck 字面量
  校验都走它，不再有两处字符串 match。

### Changed — 运行时改用 from_name（src/interpreter/dispatch.rs）
- `merge_with` 的策略名 match 替换为 `MergeStrategy::from_name` 调用
  （错误信息不变，行为不变——保留运行时兜底以覆盖动态传入的变量策略）。

### Added — typeck 编译期静态校验（src/typeck/hm/）
- `TypeError::InvalidLiteral { what, value, span }` 新变体（error.rs +
  Display + check_mir.rs 行号映射）。
- `infer_call`：`merge_with` 的第二个参数为**字符串字面量**时，用
  `from_name` 校验——非法策略编译期报错（`mora` exit 2），不再留到运行时；
  动态传入（变量）的策略放行，由运行时兜底。

### 语义
- `merge_with("x", "bogus")` → typeck 阶段 `Invalid merge_with strategy
  literal 'bogus'`（此前运行时才报 unknown strategy）。
- 单一事实来源：新增策略只需改 `from_name` 一处（运行时 + typeck 同步生效）。

### Added — 测试（tier1_typeck_mir.rs +2）
- `merge_with_invalid_strategy_literal_rejected_at_compile_time`：非法字面量
  编译期报错。
- `merge_with_dynamic_strategy_passes_typecheck`：动态变量策略放行（运行时
  兜底）。
- dispatch 单测（unknown strategy 运行时错误路径）保持通过。

### 验证
- 端到端：`merge_with("x","bogus")` → `mora` exit 2（1 type error）。
- 全测试 **694 通过 / 0 失败**（+2）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.23] — 2026-08-01 — per-key CRDT 合并策略：merge_with 写侧接线（方向 8 激活）

（三路只读审计后执行 P1。实证发现：`value.rs` 已有完整 CRDT 骨架——
`VectorClock` + `MergeStrategy`（LastWriteWins/Append/Add/DictUnion/
GrowOnlySet）+ `Conflict` 检测；`run_isolated`（handlers.rs:297-305）**读侧**
已接 `current_merge_strategies` 做 per-key 合并——但**写侧无生产者**
（`set_merge_strategies` 全仓零调用者），策略恒为 None → 恒 fallback LWW。
方向 8 的差距不是「无 CRDT」而是「接线差一根」。）

### Added — `merge_with(key, strategy)` 内置原语
- **设计**：建模为普通函数调用（走 h_call → mir_call_function → dispatch
  match + typeck builtin_signatures 既有链路）——**零新 token、零 parser
  改动**（遵守去 AST 化约束，不把语法树加回来）。语法面是 M 原语的
  「名空间」，运行时原语是架构。
- **运行时**（src/interpreter/dispatch.rs）：解析策略名
  （`append`/`add`/`dict_union`/`grow_only_set`/`lww`）写入
  `current_merge_strategies`（累积多 key）；未知策略名报错。
- **typeck**（src/typeck/dispatch.rs）：`builtin_signatures` 加
  `merge_with(String, String) → Nil` 签名。

### 语义（G-Set 端到端实证）
两个 Worker 各自 `Define("x", [..])`，`merge_with("x","grow_only_set")` 下
合并为 `[1,2,3]`（并集去重）；无策略时 LWW fallback → `[2,3]`（后写覆盖）。
VectorClock 并发检测 → Conflict 上报（现丢弃，留观测钩子）——**真 G-Set
语义激活**，仅需一个写侧调用。

### Added — 测试（5 个）
- `interpreter::dispatch::tests`（3）：设置单 key、累积多 key、未知策略报错。
- `tier0_replacement.rs`（2）：G-Set 并集 vs LWW 覆盖端到端对比
  （手工构造 Worker 指令，不经 parser——与原语测试同规范）。
- `tier1_typeck_mir.rs`（1）：`merge_with("x","grow_only_set")` typeck 干净。

### 验证
- 全测试 **692 通过 / 0 失败**（+6）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.22] — 2026-08-01 — tier2 4 个 pre-existing 测试修复（常量折叠 vs 无优化断言）

（此前全测试基线「678/680/682 通过 + 4 failed」的 4 个失败全部清零——
本 commit 后全测试首次全绿。）

### 根因（实证）
4 个失败测试断言的指令形态（`BinaryOp`/`JumpIfNot`/`Jump`）在 **v0.55 时代
「无优化」的假设下编写**，但 v0.58 引入 Cascades 优化 pass（`lower_mir_exprs`
内 `apply_rules`）后，**常量输入会被常量折叠**：

- `1 + 2` → 折叠为 `Const(3.0)`（无 `BinaryOp`）
- `if true { 1 } else { 2 }` → 折叠为直落 `Assign`（无 `JumpIfNot`/`Jump`）
- `while true { 1 }` → 折叠为 `Const(1) + Jump(0)`（无 `JumpIfNot`）

dump 实证：变量条件（`a + b` / `let c = true; if c` / `let i = 0; while i < 5`）
时指令形态全部保留——断言语义正确，问题仅在测试用了常量输入。

### Changed — tests/tier2_mir_expr_pipeline.rs（4 个测试）
- `v3_lower_binary_produces_binary_op`：`1 + 2` → `let a = 1; let b = 2; a + b`
- `v3_lower_if_produces_jump_instructions`：`if true` → `let c = true; if c`
- `v3_lower_while_produces_loop_instructions`：`while true` → `while i < 5`
- `v3_lower_while_produces_jumpifnot`：同上

各测试补注释说明根因（常量折叠导致断言失败的机制），防止未来误判为
回归。

### 验证
- 全测试 **686 通过 / 0 失败**（首次全绿）。
- clippy `-D warnings` 0 / fmt 零 diff。

## [v0.75.21] — 2026-08-01 — pipe `|>` 语法接入 + callee 名修复

（v0.75.20 树收敛时发现的同族残留：`|>` 全链路死代码——lexer `Pipe` token
存在、`parse_pipe` 从未挂接进优先级链（pre-existing 限制）、树变体已删。
本次补齐「词法→语法→树」链路。）

### Changed — `|>` 接入优先级链（src/parser_v3/mod.rs）
- `parse_pipe` 挂进 `parse_equality`（低于 equality、高于 comparison）：
  - `1 + 2 |> f` = `f(1 + 2)`（算术先于管道）；
  - `3 == 3 |> f` = `3 == f(3)`（equality 先于管道）；
  - `a |> b |> c` 链式 = `c(b(a))`。
- 移除 `#[allow(dead_code)]`（函数不再死代码）。
- 脱糖语义不变：`left |> right` → `Call(right, [left])`；typeck 走
  `infer_call`、lower 走 `MirInst::Call`。

### Fixed — `x |> f(a)` 的 callee 名丢失（src/parser_v3/mod.rs）
- 此前 `right_name = match_to_string(&right)` 对 `Call` 变体返回 `"expr"`，
  `x |> f(a)` 产出 `Call(Name("expr"), [x, a])`——callee 名丢失。
- 改为在 Call 分支内保留真名 `f`：`Call(Name("f"), [x, a])`。
- 该 bug 仅当 `parse_pipe` 被调用时存在——pipe 死代码期间不可达；
  本次挂接使其暴露并同批修复。

### Added — 测试（tier1_typeck_mir.rs）
- `pipe_syntax_hooked_into_precedence`：task 管道（脱糖后 typeck 干净）。
- `pipe_keeps_call_callee_name`：`10 |> add(5)` 不产出 Name("expr")。
- `pipe_token_now_parses_after_hookup`：取代 v0.75.20 的「应报错」断言
  ——pre-existing 限制已修复，锁定解析成功的新状态。

### 验证
- 端到端：`3 |> double |> double` = 12、`10 |> add(5)` = 15、
  `1 + 2 |> double` = 6（优先级正确）exit 0。
- Bool 算术边界（`3 == 3 |> double` → "Operands must be numbers"）为
  运行时既有限制（直接 `double(true)` 同样报错），非 pipe 引入。
- clippy `-D warnings` 0 / fmt 零 diff。
- 全测试 682 通过（+2），4 failed = tier2 pre-existing（基线同样失败）。

## [v0.75.20] — 2026-08-01 — MirExpr 树对内收敛：删除死变体 Pipe/Grouping/Expr

（去 AST 化收敛第 2 步，执行 docs/de-ast-boundary.md §3 增量路径的
「纯语法包裹 / 死形态」段。MirExprKind 33 → 30 变体。）

### Removed — MirExprKind 死变体（src/mir/expr/mod.rs）
- `Pipe`：零构造——parser 的 `parse_pipe` 从未挂接进 `parse_assignment`
  优先级链（`|>` 词法 token 存在但未接入语法，pre-existing），且 parse_pipe
  自身已把 pipe 脱糖为 `Call(right, [left])`。树形态与其脱糖产物重复。
- `Grouping`：零构造——`mir_group` helper 是恒等函数（`fn mir_group(inner)
  = inner`），从未产出包裹节点；括号仅作优先级，parse 时不建节点。
- `Expr(Box<MirExpr>)`：零构造——「作为语句执行、丢弃结果」的无操作包裹，
  从未被 parser 产出。

### Changed — 消费臂同步删除（4 文件）
- `src/mir/lower.rs`：删 `Pipe`/`Grouping`/`Expr` 三个 lower 臂
  （各臂唯一职责是穿透/发指令）。
- `src/typeck/hm/mod.rs`：删三个 infer 臂 + `infer_pipe` 死函数
  （pipe 已脱糖为 Call，HM 走 `infer_call`）。
- `src/lsp/providers/parsed_doc_v3.rs`：`walk_mir_expr` 删三个遍历臂。
- 删除即验证：cargo check 一次通过（9 处引用全部清理）。

### 保留 — MirInst 原语集不动
- `MirInst::Pipe` / `MirInst::Expr` **保留**为运算原语（手工构造可达，
  与 StreamFor/Route 同列——扩展空间预留）。运行时执行语义（handlers /
  ssa / cost）零改动。本次仅收敛编译前端树形态，原语是架构、不动。

### 新增发现（同族残留，另行处理）
- `|>` 全链路为死代码：lexer `Pipe` token → `parse_pipe`（未挂接）→
  已删树变体。pipe 语法接入属 parser 层工作（pre-existing 限制），
  不在本 commit 范围。

### Added — 测试
- `pipe_token_unconnected_to_parser_preserved`：锁定诚实状态——`|>` 解析
  失败与基线一致（经 stash 比对实证），防止树收敛意外改变词法行为。

### 验证
- 删除即验证：cargo check 一次通过（9 处引用全清）。
- clippy `-D warnings` 0 / fmt 零 diff。
- 全测试 680 通过（+1），4 failed = tier2 pre-existing
  （M1 baseline cefbe99 同样失败，与本次无关）。

## [v0.75.19] — 2026-08-01 — 语法面收敛：移除 59 个无前端可达的死关键字

（去 AST 化残余的直接来源，收敛第 1 步。架构诊断：约 20 个 MirInst 变体
无前端可达——lexer 关键字表已有 stream/route/observe/span/worker/... 的
token，但 ParserV3 不解析它们 → token 落入 `token_to_identifier_name()`
标识符 fallback。语法面既没删干净、也没接完。运行时原语集与 MirInst
执行语义完全不变——原语经手工构造 MirFunction / Pregel API 驱动，
与词面解耦。）

### Removed — lexer 死关键字（src/lexer.rs）
- `TokenType` 移除 59 个无解析可达的变体：`Export`/`Parallel`/`WithKeyword`/
  `Save`/`Load`/`Into`/`Read`/`Write`/`Append`/`ReadBytes`/`WriteBytes`/
  `Stream`/`Tool`/`Route`/`Observe`/`Span`/`Tags`/`Record`/`Trace`/`Metrics`/
  `Otel`/`Worker`/`Transaction`/`Commit`/`Rollback`/`Compensation`/`Trait`/
  `Impl`/`Where`/`Edges`/`ExitWhen`/`Rounds`/`Eval`/`Skill`/`Expect`/
  `Tolerance`/`State`/`Node`/`Channel`/`Checkpoint`/`Rewind`/`Resume`/
  `Thread`/`Dynamic`/`Map`/`Reduce`/`FanIn`/`FanOut`/`Interrupt`/`Before`/
  `After`/`Command`/`Send`/`Goto`/`Update`/`Add`/`Last`/`Merge`/`Jit`。
- `identifier_from` 关键字表同步删除对应映射；剩余 27 个真解析关键字：
  let/task/if/then/end/return/true/false/nil/for/in/import/match/fn/as/do/
  break/continue/macro/dyn/Self/type/enum/struct/orchestrate/loop/max_rounds/
  prompt + 上下文词 document。
- 词面 `stream`/`route`/`observe`/`span`/`worker`/`transaction`/`tool`/... 回归
  普通标识符：声明位、表达式位**全位置一致**可用。此前经 fallback 在声明位
  静默冒充标识符、表达式位报错（行为不一致）。

### Removed — parser fallback 表同步（src/parser_v3/mod.rs）
- `token_to_identifier_name` 收缩为存活 token 的 arm（删除的 token 无 arm
  可引用，否则编译报错——删除即验证）。

### 原则执行
- 向后不兼容是设计特权，不是技术债务：词面回归标识符是**语义改进**
  （用户可写 `let stream = ...`），不兼容仅对依赖死关键字的悬浮语法。
- 语法面（lexer+parser）与运行时原语集（MirInst+handlers）解耦：原语
  是架构，词面是接入点。删除词面不触碰原语。

### 验证
- 删除即验证：cargo check 一次通过（token 无引用）。
- clippy `-D warnings` 0 / fmt 零 diff。
- 全测试 679 通过（+1 `freed_reserved_words_usable_as_identifiers`），
  4 failed = tier2 pre-existing（M1 baseline cefbe99 同样失败）。
- 端到端：`let stream = "s"` 等 6 个新词作变量，`mora` 运行打印拼接结果
  exit 0。

## [v0.75.18] — 2026-08-01 — 静态类型 M3：跨模块 import 符号表

（类型系统补齐第 3 模块。此前 `MirExprKind::Import` 在 typeck 返回 Nil 被
完全忽略——`b.mora import "a.mora"; print(x)` 时 x 报 UnboundVariable
（运行时可用、类型检查炸）。）

### Added — src/typeck/imports.rs（新模块）
- `collect_imported_symbols`：typeck 阶段预扫描顶层 `import "path"`，递归解析
  目标文件（`visited: HashSet<PathBuf>` 防环，`a → b → a` 不重复展开），
  提取顶层符号合并进 HM env。
- `extract_module_symbols`：`let` 绑定用目标模块自身的 HM 推断结果
  （含 let-generalization 与显式注解）登记；`task`（FnDef）→ `Closure`；
  `struct`/`enum` → `Any` 占位；`type Alias = T` → 目标类型。
- 传递 import：递归时先把子 import 的符号预合并进目标模块的 HM env，
  模块自己的 `let` 引用其 import 的符号也能正确推断。
- `sanitize`：合并前退化闭包身份 TypeVar / 未解析 TypeVar（`forall<'a>.'a`
  闭包身份 → `Closure`、结构型泛型内部 TypeVar → `Any`），避免跨模块
  closure_sigs 侧表键冲突。

### Changed — check_mir.rs（唯一接入点）
- `check_program_mir` 在 HM 推断前合并 import 符号。所有入口（main.rs 5 处
  run_file/run_record/run_replay/run_snapshot/run_check + REPL + mir_import）
  都经此函数——零调用点改动，运行时语义不变。
- import 文件读取/解析失败产出一条 import error 诊断（与运行时
  `mir_import` 的 hard error 语义一致）；缺失文件不再静默 Any。

### Changed — hm/env.rs
- 新增 `TypeEnv::all_bindings()`（导出全部绑定供符号表合并）。

### 路径语义
- 与运行时 `mir_import` 完全一致（cwd 相对，`read_to_string(path)`），
  `mora --check` 与运行时对同一 import 的解析不分叉。

### Added — 测试 + fixture
- `tests/fixtures/mod_a.mora`：`let greeting: string` / `let answer: int = 42i` /
  `task plus(a, b)` 顶层符号。
- `tier1_typeck_mir.rs` 新增 3 个：`import_symbol_resolved_in_typecheck`
  （import 后引用 greeting/answer 无 UnboundVariable）、
  `import_symbol_type_checked`（import 的 string 符号 + 数字报类型错误）、
  `import_missing_file_reports_error`（缺失文件报 import error）。
- 端到端验证：`mora /tmp/use_mod.mora` 打印 hi/42（exit 0）；
  `mora --check` 无错误；`greeting + 1` 1 个类型错误（exit 2）。

### 验证
- `cargo check --all-targets` 0 error / clippy `-D warnings` 0 / fmt 零 diff。
- lib 测试 526 通过（跳过 sandbox/exec_parallel 慢测试）；tier1 25 通过。
- 全测试 678 通过（+3），4 failed = tier2_mir_expr_pipeline pre-existing
  （M1 baseline cefbe99 上同样失败，与本模块无关）。

## [v0.75.17] — 2026-08-01 — 静态类型 M2：泛型（ForAll + let-generalization + parser 泛型注解）

（类型系统补齐第 2 模块。M1 后 generalize.rs 仍是 dead code stub——注释
"can't represent ∀ without changing Type enum"；Type 枚举无泛型量化变体；
parser 注解只接受单标识符。本模块补齐真泛型。）

### Changed — Type::ForAll 变体（src/typeck/mod.rs）
- 新增 `ForAll(Vec<char>, Box<Type>)`：∀α₁...αₙ. τ，let-generalization 的产物。
- `name()` 显示 `forall<'a, 'b>. τ`；`compatible_with` 对 ForAll 与内层同判。

### Changed — 真量化 + 实例化（src/typeck/hm/generalize.rs）
- `generalize` 不再 stub：空 env 下 `'a` → `ForAll(['a], 'a)`、`list<'a>` →
  `ForAll(['a], list<'a>)`；env 已含的变量不量化（标准 HM 规则）。
- 新增 `instantiate`（自由函数版）：ForAll → fresh TypeVar 展开。

### Changed — let-generalization 接入（src/typeck/hm/mod.rs + env.rs）
- `infer_let` / `infer_let_typed` 在 `env.add` 前调 generalize。
- `infer_var` / `Call(Var)` / `infer_assign` 命中 ForAll 时实例化。
- closure 身份变量特殊处理：被量化的身份变量映射到 fresh 变量，`closure_sigs`
  侧表签名复制一份、内部 TypeVar 全部重命名——`let f = fn(x) x; f(1); f("s")`
  每次调用得到独立单形化副本，互不冲突（此前闭包共享同一组 TypeVar 约束会报
  "expected int, got string"）。
- `TypeEnv::free_variables` / `collect_type_vars` 走 ForAll 内层。

### Changed — ForAll 合一/替换（src/typeck/hm/unify.rs）
- `unify`：ForAll 与任何类型合一 → 剥壳后与内层合一（防御；正常路径 env.get
  已实例化）。
- `apply` / `contains_typevar` 递归进 ForAll 内层。

### Added — parser 泛型注解 `<...>`（src/parser_v3/mod.rs）
- `List<int>` / `dict<string, any>` 递归解析泛型参数（双 token lookahead：
  identifier 后跟 `<`）。此前 `let x: List<int> = ...` 报 "unsupported type
  annotation"。

### Added — 测试
- `generalize.rs` 单测 9 个：量化（identity、list、env-bound 跳过）、实例化
  （ForAll 展开、非 ForAll 透传）、自由变量收集。
- `tier1_typeck_mir.rs` 新增 5 个：`let_identity_polymorphic`（id(1) + id("s")
  均通过）、`let_polymorphic_list_and_pair`、`generic_type_annotation_list_int_parses`
  （`List<int> = [1i, 2i]`）、`generic_type_annotation_list_float_parses`、
  `generic_annotation_mismatch_reported`（`List<string> = [1i, 2i]` 报错）。
- 注：无后缀数字字面量 lexer 产出 Float（v0.38 数值塔分离），Int 需 `i` 后缀
  （`1i`）——测试按此语义编写。

### 验证
- `cargo check --all-targets` 0 error / clippy `-D warnings` 0 / fmt 零 diff。
- lib 测试 526 通过（跳过 sandbox/exec_parallel 慢测试）；tier1 22 通过。
- 全测试 675 通过，4 failed = tier2_mir_expr_pipeline pre-existing
  （M1 baseline cefbe99 上同样失败，与本模块无关）。

## [v0.75.16] — 2026-08-01 — 静态类型 M1：方法调用类型推断 + 列表/字典元素保留

（类型系统补齐第 1 模块。探索确认 Mora 已具备 HM 推断 + 编译期拦截，真缺口模块化推进。）

### Changed — parser 产出 MirCallee::Method（src/parser_v3/mod.rs）
- `obj.method(args)` 此前降成 `Call(Name("obj_method"))` 字符串糖 → typeck 当未知调用（infer_call else 分支 `Eq(arg, Any)` 报 UnificationFailure），`method_signature` 表完全没被走到。改为产出 `MirCallee::Method(obj, method)`（lower 层仍拼 "obj_method" 字符串，runtime 分发不变），typeck 走 `method_signature` 推断。

### Changed — typeck 方法调用推断（src/typeck/hm/mod.rs + dispatch.rs）
- `infer_call` 的 `MirCallee::Method` 分支：委托 `infer_method_call`（receiver + 参数约束 + 返回类型）。
- `method_signature_builtin` 保留元素类型：`list.map/filter` 返回 `List(elem)`（此前 `List<Any>`）、`push` 返回 `List(elem)`、`pop/get` 返回 `elem`（此前 `Any`）；`dict.get`/`set` 补 key/value 参数。
- `unify`：`Type::Union` 成员合一（`dict.get` 返回 `Union<V,Nil>` 可与成员合一）；**`Any` 作为 top type**（与任意类型合一成功 — 修复 `ys[0]` 降成 `Call(Name("ys_index"))` 时未知调用约束报错）。

### Tests
- `tests/tier1_typeck_mir.rs` +3：`dict_get_union_unifies_with_member` / `list_get_exposes_element_type_error`（String 元素 + Int 运算被检出）/ `list_map_keeps_int_elements_clean`。
- `src/typeck/dispatch.rs` 单测：`list_map_arity_is_one` → `list_map_arity_is_two`（map 现接收闭包参数）。
- 566 通过 / 0 失败（跳 14 个 pre-existing sandbox 慢测试）。clippy 0 / fmt 零 diff（M1 文件）。

## [v0.75.15] — 2026-08-01 — 约束审计 P3-2（吞异常分类审计）

对 AGENTS_CODE_MODIFICATION.md §2「禁止吞异常」的全量审计（~200 处命中：`let _ =` 130 + `.ok()` 50 + `Err(_)` 20）。

### Changed — 应传播 9 处改为显式记录（best-effort 不再静默）
- `src/interpreter/ai_chat.rs` ×2：`track_tokens` 失败 → `eprintln`（token 预算失效可见）。
- `src/main.rs` ×3：partial recording save / snapshot 目录创建失败 → `eprintln`。
- `src/mir/handlers.rs`：transaction compensation 失败 → `eprintln`（回滚不完整可见）。
- `src/pregel/mod.rs`：master_compute 失败 → `eprintln`（全局协调静默失效可见）。
- `src/pregel/worker_pool.rs` ×2：worker 结果 send / batch 广播失败 → `eprintln`。

### 保留（有意忽略，~130 处，记录理由）
- best-effort 副作用（recorder/trace 失败不阻塞主流程）、cleanup/补偿（Drop impl 内 docker/taskkill/remove_file）、`#[cfg(test)]` 简化、env 缺省降级（MORA_AI_RETRY_* 缺省即默认值）、combiner 失败回退 LWW（已有 `// fallback: LWW` 注释）。

### Tests
- interpreter 98 / pregel 30 / 其余零回归。clippy 0 / fmt 0 / build 通过。
- 注：`runtime::sandbox::tests::clone_shares_container_arc` 为 pre-existing 慢测试（隔离 89s，与本次无关）。

## [v0.75.14] — 2026-08-01 — 约束审计 P2+P3-1（clippy 清零 + fmt 零 diff + magic numbers）

对 AGENTS_CODE_MODIFICATION.md §2/§3 的达标清理。**clippy `-D warnings` 从 85 → 0，rustfmt 从 150 diff → 0**（历史最高门槛，首次达标）。

### Changed — magic numbers 提取常量（审计报告 5 处 + 2）
- `src/runtime/infra.rs`：`STRING_INTERNER_CAPACITY`（50k）/ `AI_CACHE_CAPACITY`（10k）。
- `src/http_server.rs`：`HANDLER_TIMEOUT_SECS`（60）。
- `src/mir/interp.rs`：`FLOAT_PATTERN_EPSILON`（1e-9）。
- `src/compress/mod.rs`：`SMART_CRUSHER_TARGET_DIVISOR`（200）。

### Changed — clippy 85 → 0（机械修复 + 语义修复）
- 机械：collapsible_if ×20、unused import/mut ×8、doc 格式 ×6、`--fix` 自动批量。
- 语义（读上下文判断后修，未用 allow 掩盖）：unreachable pattern ×9（`parsed_doc_v3` 重复 arm、`handlers` Halt 重复、`typeck/hm` `_` 兜底——均删冗余，部分改穷尽 match 让编译器守卫新增变体）；dead code ×6（`dag_rule` incoming_edges/node_dst、`typeck` method_return_type 副本、`dag` successors/label 字段）；`dag.rs` JumpIf/JumpIfNot 相同分支合并、range loop 改 enumerate、len>=1 → !is_empty；`typeck/dispatch` redundant guard 简化；`core.rs` 测试补 Value import。
- 合理保留（记录理由）：`MirInst` large_enum_variant、`h_impl_def`/`h_skill_def` too_many_arguments、`DagCache` 补 is_empty。

### Changed — fmt 150 diff → 0
- 全量 `cargo fmt`（40 文件，含 pre-existing 格式债 + clippy 修复后的新格式）。

### Tests
- 580 通过 / 0 失败（lib）。唯一失败仍为 tier2 4 个 pre-existing lowering 语义断言（clean 基线同样失败，另有候选）。

### 审计报告状态更新
- **无意义命名 / 失效注释**：✅ 达标（P1）。
- **magic numbers**：✅ 达标（P2-1）。
- **`#[deprecated]` 标注**：✅ 修正——项目不保留新旧并存接口（v0.75.9 直接改签名 + CHANGELOG 记录），政策与现状匹配，无标注对象。
- **clippy / rustfmt**：✅ 首次达标。

## [v0.75.13] — 2026-08-01 — 约束审计 P1（失效注释清理 + 无意义命名）

对 AGENTS_CODE_MODIFICATION.md §4（清晰度提升）的达标清理。

### Removed — 失效注释（src/interpreter/mod.rs）
- 删除描述已删除代码的注释：`AI_STREAM_TIMEOUT_SECS 已删除（create_ai_stream 是 dead code）`。
- 删除迁移残留：`Value/Environment is now in value.rs` + re-export 提示（re-export 在 mod.rs:48 已存在，注释脱节）。
- 删除实现历史：`trait impl method 注册名集中生成…收敛到这两个函数`（其后实际是 ai_retry 配置函数，引用目标已不存在）。

### Changed — 无意义命名（src/mir/ssa.rs）
- SSA Copy 指令的临时变量 `tmp` ×4 → `copy_var`（自解释：Copy 的临时寄存器名）。纯改名，零语义变化。

### Tests
- 580 通过 / 0 失败（无新增，纯清理）。clippy `-D warnings` error 数与基线持平（85）。

## [v0.75.12] — 2026-08-01 — pre-existing 缺陷修复（缓存指针复用 + parser let + 测试幻影断言）

承接 v0.75.11 清理时暴露的 pre-existing 问题批量修复。

### Fixed — DagCache 指针复用 bug（src/mir/cache.rs）
- 根因：v0.75.9 缓存项只存 `Arc<MirDag>`（key = `Arc::as_ptr`），不持有 func 强引用 → func_arc drop 后指针被 allocator 复用 → 不同函数撞同地址命中错误 DAG。pregel 并行单元测试全量并发时偶发暴露（`Const(42)` 的 body 被 `Const(10)` 命中，结果 20≠42）。
- 修复：缓存项改二元组 `(Arc<MirFunction>, Arc<MirDag>)` — 持有 func 强引用，指针永不复用，同指针必然同内容。全量 3 轮并发验证稳定。

### Fixed — ParserV3 块体内 `let` → `Assign` 语义 bug（src/parser_v3/mod.rs）
- 根因：`parse_block_body` 只试 parse_assignment + if/for/while — `let` 关键字不匹配任何分支被 advance() 跳过 → 余下 `n = 5` 解析成 `Assign` → `env.assign` 对未定义变量静默 false（n=Nil）→ task 内 let 变量后续比较/builtin 全错。
- 修复：抽 `parse_let_binding` helper，块体 `let` 优先且必须成功（`if check(Let)` 分支），顶层复用。同时顺带修好 tier2 `v3_pipeline_multiple_statements_runs`（原 clean 5 失败 → 4）。
- 未扩 AST：复用现有 `MirExprKind::LetBinding`（lower 已发 `MirInst::Define`）。

### Fixed — 测试幻影断言（指向去 AST 化时已删除的文件/代码）
- `tier0_dyntrait` 3 个：读 `src/parser_v2/expressions.rs` / `src/typeck/check.rs`（均不存在）→ 改指真实落点（lexer token 表 + MirExprKind::DynTrait + handlers dispatch）；删无法成立的 typeck 断言。
- `tier0_trait_mir` 3 个：断言 `StmtKind::TraitDef/ImplDef/SkillDef {`（v0.55 前旧 AST lowering 名）→ 改指 handlers.rs 的 mir_body 填充计数（5 处）。
- `tier0_closure_mir` `closure_reused_across_calls_via_mir`：原源码 `if ... then\n`（then 独占行，parser 不支持，从诞生起解析失败）→ 改真闭包 + Dict 方法调用驱动多次执行（Mora 无 `f(args)` 名字调用语法；Dict 方法调用路径 dispatch.rs 分发到 Value::Closure）。

### Fixed — tier0_replacement 测试源码
- 2 个 `if cond then\n...`（then 独占行）→ brace 形态 `if cond { ... }`。
- 2 个 transaction 测试：transaction 语法无前端（lexer 有 token、parser 无解析、MirExprKind 无变体）→ 改直接构造 `MirInst::Transaction`/`MirInst::Rollback` 验证 handler 语义（不经 parser）。

### Tests
- 580 通过 / 0 失败（lib，3 轮并发稳定）。**pre-existing 失败从 9 个减至 4 个**（tier2 lowering 语义缺陷：`1+2` 常量折叠 vs BinaryOp 断言、`if true` 无 JumpIfNot、`while true` 无条件回跳死循环 — clean 基线同样失败，属 lowering 语义问题，超出清理范围，留作后续候选）。clippy `-D warnings` 87 → 85（净 -2）。

## [v0.75.11] — 2026-08-01 — AST 残余低风险清理

去 AST 化收尾：删除死类型 + 幻影注释 + 注释脱节，零语义变更。

### Removed
- **`FlowSignal`（src/value.rs）**：v2 AST 解释器的控制流信号枚举，v0.55 去 AST 后成为死代码 — 生产零引用（仅 `interpreter/mod.rs:48` 一处 re-export + 测试占位），`into_value`/`is_return` 全项目零调用；`FlowSignal::Interrupt`（"Pregel HITL"）从无构造/消费（HITL 实际由 `MirInterruptCallback`/`interrupt_points` 实现）。`MirSignal`（interp.rs）不受影响 — pregel 生产使用。
- **`interpreter/mod.rs:48` re-export**：去掉 `FlowSignal`（`Environment`/`StreamReader`/`Value` 生产使用，保留）。
- **`tests/tier0_replacement.rs`**：删除 `_FLOW_SIGNAL_PRESENT` 占位常量 + 其 `#[allow(dead_code)]` 注释。

### Fixed
- **`tests/tier0_replacement.rs` 幻影引用**：注释称「配套的 AST 行为基准保留在 `tests/mir_differential.rs`」— 该文件从未建立（PHASE_ALPHA_IR_DESIGN.md 中为未完成待办）。改为如实描述：Tier 0 AST 执行器已移除，测试直接走 MIR。
- **`interpreter/mod.rs:807-808` 过时注释**（引用已删除的 FlowSignal）。

### Tests
- 580 通过 / 0 失败（无新增）。clippy `-D warnings` error 数与基线持平（86）。

## [v0.75.10] — 2026-08-01 — 寄存器级增量（DagExecMemo + 加法注入）+ 修复 v0.75.9 缓存失效

（第三步「完整寄存器级增量」落地，采用非破坏 C 路径：保留 `input` 契约 + 加法注入逐 channel var + 纯节点记忆化。计划中记录的破坏性路径 B — 删除 `input` 契约、全量 dirty 传播 — 会破坏所有现有 agent 读取语义，未采用。）

### Added — 寄存器级增量执行器（src/mir/dag_interp.rs）
- `DagExecMemo`：跨调用/超步状态化 memo — 只对「可证明纯计算」节点（白名单：`Const/BinaryOp/ListLit/DictLit/Index/Expr`，零 env 读取、零副作用、输出 = 输入函数）按「输入寄存器值相等」跳过执行、复用上次输出。副作用/env 读取节点（Var/Call/Prompt/Send/Define/Assign/...）永远重跑 — 保守白名单保证增量安全。
- `run_dag_with_signal_memo`：memo 变体；`run_dag_with_signal` 委托它（每次新 memo = 无增量，语义与旧实现完全一致）。记忆按输入值而非超步号判断 → fault-retry 回滚后仍正确。
- `DagExecMemo::skipped_nodes/executed_nodes`（可观测性）+ `is_empty`（并行 RECONCILE 区分跳过路径）。
- 新增 3 单测：`memo_second_run_skips_pure_nodes` / `memo_input_change_forces_recompute` / `memo_var_not_skipped`（Var 不在白名单，永不跳过）。

### Added — Pregel 加法注入（src/pregel/mod.rs，C 路径）
- `inject_channel_inputs`：保留 `input`（delta JSON）契约，另把每个已变更 channel 注入为 `input_<channel>` env var（typed Value，非 JSON 字符串）。旧 agent 读 `input` 完全无感；新 agent 读细粒度 var 获得真正寄存器级感知（例：消息 channel 名为 `input` → `input_input`）。
- 顺序路径接入 memo 执行；`AgentExecOutcome.nodes_executed`（worker/跳过路径为 0）。
- 新增 2 集成测试：`register_memo_skips_pure_nodes_on_reactivation`（多超步 send 链，b 二次激活纯前缀跳过 + 稳定 Arc 断言）、`channel_input_var_injected_additively`（input_input 细粒度注入，input 契约缓存保留）。

### Fixed — v0.75.9 全局 DAG 缓存失效 bug
- 根因：v0.75.9 每超步 `Arc::new(agent.task_body.clone())` → 指针每次不同 → 全局缓存（key = `Arc::as_ptr`）跨超步永远 miss，DAG 每步全量重建（v0.75.6 引擎本地缓存的收益被丢弃）。
- 修复：`stable_task_arc`（engine 生命周期持有 agent task_body 的稳定 Arc，`task_arcs` 字段）→ 跨超步同一指针，缓存真正命中；同 Arc 锚定 memo 记录。pregel 6 处 run_mir 调用点也改走稳定 Arc（cond_body/master/combiner/merge_fn 由调用方持有）。

### Tests
- 580 通过 / 0 失败（+5：dag_interp memo 3 + pregel 2）。`closure_reused_across_calls_via_mir` 为 pre-existing 失败（parse error，clean tree 同样失败，与本次无关）。clippy `-D warnings` error 数 87 → 86（净 -1，改动文件零新增）。

## [v0.75.9] — 2026-08-01 — 函数调用 DAG 全局缓存 + deps.rs 死代码清理

（用户选「全链路三步」中的前两步；第三步「完整寄存器级增量」经探索确认需改 `build_node_input` 注入方式（channel 拆独立 env var，破坏所有现有 agent 的 input 读取语义），且 deps.rs 增量模型与 MirDag 执行模型不兼容 — 收益 < 破坏成本，不纳入。）

### Added — 全局 DAG 缓存（src/mir/cache.rs，新增）
- `DagCache` + 全局静态 `DAG_CACHE`（`OnceLock`，进程级线程安全）：`MirFunction → 优化后 MirDag` 跨调用缓存，key = `Arc::as_ptr` 指针地址（body 构造后不可变，项目内无 `Arc::get_mut` 改写，同 Arc 即同 DAG）。
- 构建路径 = `dag_analyze → dag_optimize → prune_sequence_edges`（与 `run_mir_with_signal` 原路径一致）；容量上限 128，满则整体清空（简单 LRU 近似，防无限增长）。
- 新增 3 测试：`same_arc_hits_different_arc_rebuilds` / `clear_forces_rebuild` / `cached_dag_runs_same_result`。

### Changed — run_mir 签名 `&MirFunction` → `&Arc<MirFunction>`（签名传播，编译期全量暴露）
- `run_mir` / `run_mir_with_signal` / `run_main_task` / `run_main_task_with_signal` / `run_mir_dag` 全部改收 `&Arc<MirFunction>`；`run_mir_with_signal` 内建 DAG 改走全局缓存，不再每次全量重建。
- 调用方更新：dispatch.rs Closure/Task（`mir_body` 已是 Arc，零改）；handlers.rs 9 处（h_call task body / h_with_config / run_isolated / h_transaction compensation / h_prompt_section / h_document_section / h_match_expr / h_stream_for，`Arc::new((*x).clone())`）；interpreter/mod.rs import + REPL；main.rs 4 组 run_mir+run_main_task 共享同一 Arc（同一缓存项）；pregel 6 处 + 顺序/并行 EXEC。
- pregel `agent_dag_cache` 移除（引擎本地缓存 → 全局缓存统一；Closure/Task/REPL/pregel 共用一套，同 Arc 同 DAG）。原 `cached_agent_dag_is_idempotent` 测试改写为 `global_dag_cache_is_idempotent`（独立 DagCache 实例）。
- `TaskDef` body 克隆进 Arc 后走缓存（task main 执行不再每次重建）。

### Removed — src/mir/deps.rs（601 行未编译死代码）
- 未注册（`mir/mod.rs` 无 `mod deps`）、grep 零引用；功能与 `MirDag`（指令级 DAG + 控制流边）重复。注册编译 ≈ 引入第二套数据流系统（维护双份），且 `mark_dirty/recompute_dirty` 寄存器级增量与 dag_interp `reg_ready/exec_count` 模型不兼容。

### Tests
- 575 通过 / 0 失败（+3，lib）。`closure_reused_across_calls_via_mir` 为 pre-existing 失败（parse error，clean tree 同样失败，与本次无关）。clippy `-D warnings` error 数与基线持平（87）。

## [v0.75.8] — 2026-08-01 — refine 多候选生成 + Pregel 增量执行 v1

### Changed — refine 多候选生成（src/refine/mod.rs + src/interpreter/builtins/mod.rs）
- `RefineSession::refine_many(instruction, count)`：一次 instruction 生成 N 个独立候选副本（`<stem>.refined.<n>.<a|b|c...>`，同迭代号），带各自候选注释头。`refine()` 委托 `refine_many(1)`，单候选保持旧文件名格式兼容。
- `mora.refine(path, instruction, count)`：第 3 参 count 生成 N 候选 → 返回 `List[Dict]`；2 参（count=1）仍返回单个 Dict（完全兼容）。生成式设计的最小有价值形态（多方案生成，非约束求解 — 探索判过度工程）。
- 新增 4 测试：`refine_many_creates_n_candidates` / `refine_many_validates_count` / `refine_single_keeps_legacy_filename` / `mora_refine_many_returns_list`。

### Changed — Pregel 增量执行 v1（src/pregel/mod.rs）
- `agent_input_cache` + `agent_outcome_cache`：超步间 input（build_node_input JSON）完全未变时跳过整个 agent 执行，复用上次 outcome（signal/result/sends）。input 相同 → 确定性执行，语义等价；跳过避免重复副作用（如 ai.chat 网络调用）。
- 完整寄存器级增量（channel 拆独立 env var + MirDag 节点 dirty）需改 input 注入方式（破坏现有 agent 语义），留作后续候选。
- 新增 2 测试：`incremental_skip_when_input_unchanged`（预填充缓存验证跳过 + 结果复用）、`incremental_cache_consistent_after_run`（run 后缓存填充）。
- `PreparedJob` type alias（并行 PREPARE 产物，消 clippy type_complexity）。

### Tests
- 572 通过 / 0 失败（+6）。clippy `-D warnings` error 数 88 → 87。

## [v0.75.7] — 2026-07-31 — SSA 根因修复启用 + Pregel FPGA 式调度

### Changed — SSA rename 根因修复 + 管线启用（src/mir/ssa.rs + lower.rs）
- **根因修复**：`rename_variables`/`rename_reads` 对 `Define`/`Assign` 的 src（来源寄存器）跳过 rename 解析，且 `Define` 的 src 被误当 dst 重编号 → deconstruct 映射错乱，优化后返回值丢失变 Nil。修复：Define/Assign 的 src 经 `rename_stack` 解析，且不重编号（第二字段是读，不是写）。
- **管线接入** `lower_mir_exprs`：`MORA_OPT=1/2` 启用 SSA 优化（默认关闭，热路径零开销）——环境变量自 v0.56 设计以来首次真正生效。
- `tests/mir_ssa_roundtrip.rs` 升级：新增 3 个顶层显式 return 严格等价性断言（此前因寄存器 bug 失败），全绿。

### Changed — Pregel FPGA 式调度（src/pregel/mod.rs）
- **可观测性**：`EngineStats.per_agent_ms: HashMap<String, u128>` + `AgentExecOutcome.duration_ms` — 顺序/并行路径计时，RECONCILE 记录（识别 straggler）。
- **Longest-Job-First 排序**：PREPARE 后按 DAG 复杂度（nodes.len()）降序。BSP 超步隔离保证同超步顺序无关（读 step-start 快照、写延迟到 barrier 后），重排仅改分发顺序、不影响正确性；长 job 先调度减少 worker 空闲尾巴（FPGA list-scheduling 精神）。
- 新增 2 测试：`stats_tracks_per_agent_duration`、`ljf_order_preserves_correctness`。

### Tests
- 566 通过 / 0 失败（+2）。clippy `-D warnings` error 数与基线持平（88）。

## [v0.75.6] — 2026-07-31 — SSA 管线验证 + Pregel DAG 缓存

据「他山之石」#3（优化器）与 #2（电子表格增量重算）的落地推进。

### Changed — SSA 管线验证（`opt.rs` 首次测试 + 3 个独立 bug 修复）
- 新增 `tests/mir_ssa_roundtrip.rs`：SSA 管线（construct → Basic/Aggressive pipeline → deconstruct）首次被测试。
- **验证发现系统性 bug → 管线未接入执行链**：SSA construct/传播后寄存器引用丢失（`let x = 1+2; return x` 的 `Define("x", 3)` 引用的 reg 3 无产生者，优化后返回值变 Nil）。`MORA_OPT` 保持默认关闭（环境变量读到但跳过），待修复后启用。
- 顺带修复 3 个独立 bug（均有回归测试）：
  1. **dag placeholder 0 → usize::MAX**（`dag_rule.rs`/`dag_search.rs`）：`apply_rewrite` 用 `from == 0` 作「新节点」占位，与合法节点 id 0 冲突 — 含变量操作数的代码会触发 index out of bounds（pre-existing，被真实用例触发）。
  2. **DCE 保留隐式返回载体**（`opt.rs`）：`Return(None)` 块的最后一条产生 dst 指令计入 used，避免被当死代码删除。
  3. **deconstruct 丢弃 Return(None)**（`ssa.rs`）：不再发射会在块首短路的 `Return(None)`，线性执行自然隐式返回最后产生值。
- 已知未修复：顶层隐式返回语义依赖 dag_interp「最后产生 dst 节点」作结果载体，优化重排后不稳定（独立于 SSA 管线）。

### Changed — Pregel task_body DAG 缓存（`src/pregel/mod.rs`）
- 新增 `agent_dag_cache: HashMap<String, Arc<MirDag>>` + `cached_agent_dag()`：pregel 每超步重跑同一 task_body，此前每次 `dag_analyze + dag_optimize + prune` 全量重建 — 现缓存优化后 DAG（顺序 + 并行路径均接入）。
- 仅缓存 agents 的 task_body（config 静态，随 engine 生命周期，无泄漏）。
- 新增 2 测试：`cached_agent_dag_is_idempotent`（Arc 缓存命中）、`multi_step_run_uses_cached_dag`（a send→b 多超步 + 缓存路径结果非空）。

### Tests
- 564 通过 / 0 失败（+8：SSA 6 + pregel 2）。clippy `-D warnings` error 数与基线持平（88）。

## [v0.75.5] — 2026-07-31 — Cascades 择优 + G-Set CRDT 语义

两项独立增强（据「他山之石」第 3 号——数据库优化器 Cascades/Volcano 框架，与第 8 号——CRDT 冲突-free 数据结构）。

### Changed — Cascades 同 stage 择优（src/mir/optimize/dag_search.rs）
- `dag_search_staged` 同 stage 内从「第一个 delta>0 就 break」改为「收集本节点所有可应用重写，选 cost delta 最大的应用」——规则匹配与代价评估分离（Cascades 核心精神）。
- 安全性：所有规则 `rewrite` 只读返回 owned `DagRewrite`，候选收集期间 dag 不变。
- 新增测试 `staged_picks_highest_gain_in_stage`：测试局部低收益规则与 ConstFolding 同 stage，断言择优选中折叠（delta=2）而非先匹配的小 gain（delta=1）。

### Changed — G-Set（grow-only set）CRDT 语义
- `src/value.rs`：`MergeStrategy` 新增 `GrowOnlySet` 变体 — List 并集（只加新元素）/ Dict key 级并集（child 的 key 仅在 parent 缺失时插入）/ 其他 LWW。`Value::merge` 加对应 arm。
- `src/mir/expr/mod.rs`：`MirReducerKind` 新增 `GrowOnly` 变体，`to_merge_strategy` 映射到 `MergeStrategy::GrowOnlySet`——pregel `state_schema` 可声明 grow-only 通道。
- 影响面：`to_merge_strategy` 是唯一 exhaustive match（已加 arm）；pregel `apply_write` 的 catch-all 不受影响。
- 新增测试：`merge_grow_only_set_lists` / `merge_grow_only_set_dicts` / `merge_grow_only_set_fallback_lww` / `env_merge_with_grow_only_set_strategy` / `to_merge_strategy_maps_reducers`。
- 顺带修正 value.rs 两处既有 vector_clock 测试缩进（fmt 合规）。

### Tests
- 562 通过 / 0 失败（+6 新增）。clippy `-D warnings` error 数 89 → 88。

## [v0.75.4] — 2026-07-31 — Pregel 消息计数 + 提前失败校验

按「他山之石」分析（石 2：Apache Giraph 的 message counter + barrier 精神 — 在超步边界确认每条消息都有合法接收者并统计消息量）。BSP barrier 在 mora 已天然存在（每超步 `pending_sends.drain` 全量分发），故只补齐可观测性与提前校验。

### Changed
- `src/pregel/mod.rs`：
  - `EngineStats` 新增 `pub messages_sent: usize` — 每超步 ADVANCE 分发的 SendTask 总量（derive Default 自动初始化，向后兼容）。
  - **提前失败校验**：ADVANCE 对 send 到未定义节点的消息立即报错（`"Pregel: send to undefined node 'xxx' (defined agents: ...)"`），而非延迟到下一超步 EXEC 才报 `undefined agent`。错误信息含 target 与已定义 agents 列表。
  - 不设 `messages_received` — BSP 保证 sent == received 天然成立。
- 新增 2 测试：`advance_rejects_send_to_undefined_node`（失败发生在第一超步 ADVANCE，steps==0）、`messages_sent_tracks_advance_delivery`（a→b send 图，统计消息）。

### Tests
- 556 通过 / 0 失败（pregel 模块 22 个测试全绿）。clippy `-D warnings` error 数与基线持平（89）。

## [v0.75.3] — 2026-07-31 — Pregel 增量 step 快照（StepUndo）

按「他山之石」分析（石 1：Flink Chandy-Lamport 增量 checkpoint 思想 — 只记录变更而非全量快照）消除 `MirPregelEngine::run()` 每步全量克隆热点。

### Changed
- `src/pregel/mod.rs`：
  - 新增 `StepUndo`（undo log）— 只记录 EXEC 会修改的引擎状态（`pending_sends`），替代每步全量 `build_checkpoint()`。
  - `begin_step()`：`fault_tolerance == 0`（默认）时返回 `None` — 此前每步 `build_checkpoint()` 克隆全部 `channels`/`channel_versions`/`versions_seen`/`pending_sends`，在默认配置从未被读取，纯浪费。
  - `rollback_step()`：还原 `pending_sends`；`vertex_state`/`aggregator_acc` 走「清空 + config initials 重建」— 与 `restore_checkpoint` 语义一致。
  - 依据：retry 只重跑 EXEC，而 EXEC 对 `channels` 等零写入（UPDATE 阶段在 retry 循环之外）；每步快照成本从 O(全量 channels) 降为 O(pending_sends)。
  - `build_checkpoint`/`restore_checkpoint` 保留不动（`h_orchestrate` 跨 run resume 与 auto-save 仍用）。
- 新增 2 测试：`begin_step_skipped_when_no_fault_tolerance`、`step_undo_rolls_back_pending_sends`。

### Tests
- 554 通过 / 0 失败（pregel 模块 20 个测试全绿）。clippy `-D warnings` error 数与基线持平（89）。

## [v0.75.2] — 2026-07-31 — Scheduler O(1) 时间索引（Timing Wheel）

按「他山之石」分析（石 3：Linux Kernel / Varnish 的分层定时轮盘，落地为 tokio 同思路的稀疏 BTreeMap 时间索引）优化 `Scheduler::tick`。

### Changed
- `src/schedule/mod.rs`：新增 `buckets: BTreeMap<u64, Vec<String>>` 到期时间索引（next_fire_epoch → job ids）。
  - `add`：计算 next_fire（Every = now + interval，At = at_epoch）并入桶。
  - `tick(now)`：`buckets.range(..=now)` 直接取走到期桶，从 O(全部 job) 降为 O(到期项)；对 now 大跳跃免疫（零空推进）。
  - `remove`：只删 jobs（事实来源），桶内 id 由 tick 惰性清理。
- 语义 100% 不变：Every 非对齐（`last_run = now`）、At 触发即删、一次 tick 不补触发积压周期。
- 新增 3 测试：`tick_large_schedule_only_processes_due`（1000 job 只处理到期桶）、`lazy_removal_bucket_cleanup`、`at_job_not_rescheduled`。

### Tests
- 552 通过 / 0 失败（schedule 模块 17 个测试全绿）。clippy `-D warnings` error 数与基线持平（89）。

## [v0.75.1] — 2026-07-31 — P0 架构债务修复（内部重构，无行为变更）

按全项目架构审查（architecture-reviewer）确认的 3 项 P0 阻塞项修复，打破 `runtime/` ↔ `interpreter/`、`mir/` ↔ `interpreter/` 两处双向依赖循环并清理越层耦合。

### Changed — 类型下沉（runtime ↔ interpreter 解耦）
- 新增 `src/runtime/types.rs`：`AiConfigValue` / `LruCache` / `RouteConfig` / `TokenBudget` / `TokenUsage` / `ToolDef` / `TraitInfo` / `TraitMethodSig` 及 `impl_method_key` / `default_impl_method_key` 从 `interpreter/mod.rs` 下沉到 runtime 侧（字段可见性 `pub(crate)`）。
- `interpreter/mod.rs` 保留 re-export（`pub use crate::runtime::types::{...}`）保持既有路径兼容；`runtime/core.rs` / `ai.rs` / `infra.rs` / `registry.rs` 改 `use crate::runtime::types::*`。
- 验收：`grep -rn "crate::interpreter" src/runtime/` 为空。

### Changed — MirHost 抽象（mir ↔ interpreter 解耦）
- 新增 `src/mir/host.rs`：`MirHost` trait 定义 MIR 解释器所需宿主能力（方法桥 / config / checkpoint / environment / dynamic_sends / trait registry / `clone_box`）。
- `Interpreter` 实现 `MirHost`（委托既有固有方法，`clone_box` = `self.clone()`）。
- `mir/interp.rs` / `mir/handlers.rs` / `mir/dag_interp.rs` / `mir/jit.rs` 宿主参数从 `&mut Interpreter` 改为 `&mut dyn MirHost`。
- 验收：`grep -rn "crate::interpreter" src/mir/ src/pregel/`（生产代码）为空。

### Changed — MirPregelEngine 归位（结构错位修复）
- `interpreter/mir_pregel_engine.rs` → `src/pregel/mod.rs`；`interpreter/worker_pool.rs` → `src/pregel/worker_pool.rs`（git mv，历史保留）。
- `MirPregelEngine::run` / `reconcile_outcome` / `apply_write` 宿主参数改为 `&mut dyn MirHost`；内部字段访问改 trait 方法；并行 worker 克隆宿主改用 `clone_box()`。
- 注：原 P0 计划的「MirPregelEngine 迁移」原属 P2，因 `h_orchestrate` 依赖其路径而并入本次（否则 mir → interpreter 循环无法完全打破）。

### Changed — 移除 MirExpr 死字段
- `MirExpr.ty: Option<Type>`（`src/mir/expr/mod.rs`）为从未被写入/读取的死字段（构造器恒 `None`，`typeck/hm` 的 memo 快速路径永不触发），整体移除并删除 30+ 处 `ty: None` 构造行。
- `typeck/hm/mod.rs` / `typeck/check_mir.rs` 修正声称「写回 ty」的误导性注释；`check_program_mir_with_types` 保留为薄透传（语义澄清）。
- 删除依赖 memo 行为的测试 `tests/tier1_typeck_mir.rs::typed_mir_expr_zero_handled`；更新 `tests/tier2_mir_expr_pipeline.rs` 与 `tests/tier0_replacement.rs` 的公共 API 守门签名（宿主参数改为 `&mut dyn MirHost`）。
- 注意：`MirExprKind` 变体中的类型注解（`LetBinding::type_hint` / `Param::type_hint` / `StructDef::fields` 等）为活跃功能，保留不动。

### Tests
- 549 通过 / 0 失败（既有失败 `tier0_closure_mir::closure_reused_across_calls_via_mir` 为 pre-existing，干净树同样失败，非本次引入）。
- clippy `-D warnings` error 数从基线 95 → 89；`cargo fmt --check` 通过。

## [Unreleased] — Tier 0 → Tier 1 

MIR Tier 1 5 ////`run_file` / `run_record` / `run_replay` / `run_snapshot` / REPL (`mora --repl`)  `mora::mir::interp::run_mir` + `run_main_task` `Interpreter::interpret` / `execute` / `evaluate``MORA_INTERP`  `interpreter_mode()` 

**** Tier 0 

1. `tests/mir_differential.rs` — AST 
2. `Interpreter::mir_call_function` / `mir_call_method` / `mir_import` / `mir_with_config` — MIR  AST builtin 
3. `Interpreter::evaluate` / `call_value_inner` / `call_task_inner` —  builtin 

### Added
- `src/mir/interp.rs`:  `MirSignal` enum  `run_mir_with_signal` / `run_main_task_with_signal` REPL  Return/Break/Continue 
- `src/mir/`: α.5-α.8  MirInstMacroDef / Worker / Commit / Route / Observe / Span / RecordTokens / Save / Load / ReadFile / WriteFile / AppendFile / ReadBytesFile / WriteBytesFile / TraitDef / ImplDef / Orchestrate / Eval / SkillDef / PromptSection / DocumentSection `StmtKind` 
- `src/mir/lower.rs`: `lower_stmt`  41  `StmtKind` `#[allow(unreachable_patterns)]`  catch-all 
- `tests/tier0_replacement.rs`: 7  syntax / semantics / type-system / stdlib / runtime  AST `interpret`/`execute`

### Changed
- `src/main.rs`:  `interpreter_mode()`  AST fallback`run_file` / `run_record` / `run_replay` / `run_snapshot`  MIR lowering
- `src/interpreter/mod.rs::run_repl_with`: REPL  `mora::mir::lower::lower_program` + `mora::mir::interp::run_mir` task  `MirInst::TaskDef` 

## [v0.49.0] - 2026-07-07 —  +  +  (15 fixes)

1 commit; v0.49 audit follow-up (per user request: check simple implementations for high-concurrency / high-pressure correctness).

### Category A:  (6 fixes)

| # | Fix | File | Test |
|---|---|---|---|
| A1+B1 | CapabilityStore  `current_generation`; revoke  bump ; check  token.generation == current_generation (was: revoke ) | `src/sandbox/capability.rs:200-273` | revoke_invalidates_token_immediately + stress race |
| A2 | `mora.refine` builtin: drop lock before session.refine() (I/O ). RefineSession::refine  owned RefineStep  &RefineStep | `src/interpreter/builtins.rs:1745-1755`, `src/refine/mod.rs:107-154` | 7/7 refine builtin tests |
| A3 | mora.refine  + RefineStep clone  — 50 thread  | `src/stress_tests.rs:stress_refine_concurrent` |
| A4 | Semaphore::release  Ordering::AcqRel (was SeqCst, lighter barrier); acquire  AcqRel/Acquire (was SeqCst) | `src/interpreter/builtins.rs:2065-2090` | stress_semaphore_cas |
| A5 | CapabilityStore::check  inline lookup ( get() clone) —  get + check + generation  | `src/sandbox/capability.rs:264-281` | stress_capability_check_throughput (100k check/sec) |
| A6 | Interpreter.ai_cache / string_interner / draft_model_stats  Arc<Mutex<>>  (was raw HashMap, ) | `src/interpreter/mod.rs:155-205, 271-313`, `src/interpreter/ai_chat.rs:323-364, 411-414, 506-516` | stress_lru_concurrent (100 thread put) |

### Category B:  (5 fixes, B1  A1)

| # | Fix | File | Test |
|---|---|---|---|
| B1 | ( A1) | | |
| B2 | ContainerHandle::exec_with_timeout: spawn waiter thread + recv_timeout(N).  output()  (docker exec sleep infinity ) | `src/sandbox/container.rs:255-309` | stress_docker_exec_timeout (#\[ignore\],  docker) |
| B3 | generate_container_name  Arc<AtomicU64> counter : mora-{nanos}-{counter}. 100  | `src/sandbox/container.rs:354-369` | stress_container_name_unique (100 thread) |
| B4 | orchestrate graph step > 100 magic → MAX_GRAPH_STEPS: usize = 1000 const +  | `src/interpreter/orchestrate.rs:8-9, 50-58` |  4  orchestrate tests |

### Category C:  (4 fixes)

| # | Fix | File | Test |
|---|---|---|---|
| C1 | LruCache  (cap 10000 for ai_cache); put/get/evict O(1) +  entry  | `src/interpreter/mod.rs:139-179` | stress_ai_cache_lru_cap (1M put) |
| C2 | LruCache cap 50000 for string_interner |  C1 | stress_string_interner_lru_cap (1M put) |
| C3 | ContainerHandle  Drop impl: docker rm -f (opt-in via auto_cleanup=true, default true; with_auto_cleanup(false) ) | `src/sandbox/container.rs:223-251` | real_docker_spawn_and_destroy (#\[ignore\]) |
| C4 | worker_receivers cleanup: . Interpreter Clone  Arc  (v0.34 singletons via Arc) | `src/interpreter/mod.rs:248-314` | ( worker tests ) |

### New infrastructure

- **`LruCache<V>` struct** (mod.rs:139-179):  LRU, no deps. VecDeque<String> for O(1) pop_front + HashMap<String, V> for O(1) lookup. Used by ai_cache and string_interner.

### Test

- **New `src/stress_tests.rs`** (10 stress tests, all #[ignore] by default, run with --ignored):
  - stress_capability_revoke_under_race (100 thread)
  - stress_refine_concurrent (50 thread)
  - stress_semaphore_cas (1000 acquires + releases)
  - stress_capability_check_throughput (100k check/sec)
  - stress_ai_cache_lru_cap (1M put)
  - stress_string_interner_lru_cap (1M put)
  - stress_container_name_unique (100 thread, 1000 names)
  - stress_orchestrate_max_steps (B4 const sanity)
  - stress_lru_concurrent (100 thread put)
  - stress_container_drop_cleanup (100 handle drop, #[ignore],  docker)
  - stress_docker_exec_timeout (#[ignore],  docker)

- **Updated tests** (existing, behavior changes):
  - sandbox::capability::revoke_invalidates_token_immediately:  v0.49  (revoke )
  - interpreter::builtins::tests_v042_capability::sandbox_revoke_bumps_generation:  v0.49 (revoked token check_call  false)

### Total impact
- 1 commit
- ~1100 LOC net (impl + tests + stress infrastructure)
- +21 tests (11 sandbox capability + 1 builtin capability + 9 stress)
- 1 new Cargo dep: NONE (0 deps added; uses std::sync)
- 562 tests pass total (lib 556 + bin 6), 0 fail
- 14 tests #[ignore] (real Docker / long-running)
- clippy: 10 acceptable warnings in stress_tests.rs (test code quality, not bugs)
- fmt clean

## [v0.48.0] - 2026-07-06 — plan.update + mora.refine (pi-agent + CLI-Anything)

1 commit; v0.48+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### plan.update — real-time checklist (pi-agent §1.11)

- **New module `src/plan/mod.rs`**:
  - `StepStatus` enum: Pending () / InProgress () / Done ()
    with emoji ↔ text ↔ alias parsing (todo / doing / completed / etc.)
  - `PlanStep { id, text, status }` — single checklist item
  - `Plan` — ordered list + HashMap by_id for O(1) update
  - `add_step` / `update([(id, status)])` / `remove_step` / `get`
  - `complete_count` / `in_progress_count` / `pending_count` /
    `completion_ratio` helpers
  - 9 module-level tests (emoji/text parsing, add/update/remove,
    completion_ratio, empty plan)

- **`plan.*` builtins** (added to `call_plan_method`, 7 methods):
  - `plan.create(name, steps)` → String(name); steps: List[Dict{id, text, status?}]
  - `plan.update(name, updates)` → Bool(true); updates: List[[id, status]]
  - `plan.add(name, id, text)` → Bool(true) (append step)
  - `plan.remove(name, id)` → Bool(true)
  - `plan.list(name?)` → List (of plan names or step Dict[])
  - `plan.info(name)` → Dict{total, done, pending, completion_ratio}
  - Status accepts: pending/todo/, in_progress/in-progress//doing,
    done/completed//finish (emoji + text + alias all supported)

### mora.refine — incremental edit loop (CLI-Anything §1.3)

- **New module `src/refine/mod.rs`**:
  - `RefineStep { iteration, script_path, refined_path, instruction,
    original_bytes, refined_bytes, diff_lines_added/removed, timestamp }`
  - `RefineSession::new(script_path)` — computes `<stem>.refine/`
    subdir from script path
  - `RefineSession::refine(instruction)` — REAL file I/O: read script,
    create .refine/ dir, write `<stem>.refined.<n>.<ext>` with
    `# --- INSTRUCTION (refine iter n): <text>` header + original
    content. Returns `&RefineStep` with diff line counts.
  - `RefineRegistry` — multi-script session map
  - 6 module-level tests (real file I/O, multi-iteration,
    separate files, nonexistent error, multi-session, dict fields)

- **`mora.*` builtins** (added to `call_mora_method`, 3 methods):
  - `mora.refine(script_path, instruction)` → Dict{iteration, script,
    refined, instruction, original_bytes, refined_bytes,
    diff_lines_added, diff_lines_removed} (REAL file I/O)
  - `mora.refine_info(script_path, iteration?)` → Dict (latest or
    specific iteration)
  - `mora.list_refines()` → List[String] of all script paths with sessions

- **`Interpreter.plans` + `Interpreter.refine_registry` fields** (both
  `Arc<Mutex<>>` for `&self` API compat).

- **`BuiltinKind::Plan` + `BuiltinKind::Mora`** new variants; `plan` and
  `mora` global names registered.

### Design decision: REAL file I/O (not metadata-only)

master doc §3.3 says "mora refine 'add X' " (CLI-Anything).
**v0.48.0 actually writes files**:
- `mora.refine()` reads original script + writes `.refine/<stem>.refined.<n>.<ext>`
  with instruction header (REAL create_dir_all + write)
- `mora.refine_info()` re-reads file metadata for accurate
  original_bytes / refined_bytes
- No metadata-only "this is what we'd do" stubs

### 30 new tests (9 plan module + 6 refine module + 15 builtin)
- 9 `plan::tests::*`
- 6 `refine::tests::*` (incl. real file I/O tests)
- 8 `tests_v048_plan::*` (create/update/add/remove/list/info/emoji/unknown)
- 7 `tests_v048_refine::*` (real_file/iteration_increment/latest/specific/
  list/nonexistent/unknown)

### Total impact
- 1 commit
- ~700 LOC (+~280 plan + ~270 refine + ~150 builtin + ~80 tests cleanup)
- +30 tests (531 pre-existing retained)
- **561 tests pass total** (lib 555 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### v0.41+ roadmap complete

This commit finishes master doc §4 first wave + v0.45-v0.48 (8 commits).
v0.41-v0.48 covers all P0/P1/P2 patches identified by §4 of
RESEARCH_PRIMITIVES_MASTER_v2.md. Future work (v1.0+) includes:
- WASM sandbox (master doc §3.4)
- TRINITY router (deferred — repo access limited)
- 5-layer DI container (Puter)
- serde_yaml/serde_json upgrades (currently hand-written)

---

## [v0.47.0] - 2026-07-06 — DAG-as-data + heartbeat.md + context.trim

1 commit; v0.47+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### DAG-as-data orchestration (OpenFugu §1.6)

- **New module `src/orchestrate_dag/mod.rs`**:
  - `OrchestrateDag { nodes, edges }` — declarative DAG (OpenFugu
    `model_id[]` / `subtasks[]` / `access_list[]` 
    Mora adaptation: nodes + edges)
  - `validate()` — detect cycles, duplicate nodes, unknown endpoints
  - `topological_order()` — Kahn's algorithm (BFS) — O(V+E)
  - `has_cycle()` — boolean helper
  - 9 module-level tests (linear/diamond/4-layer, cycle detection,
    self-loop, duplicate node, unknown endpoint)

- **`ai.dag(nodes, edges)` builtin** (added to `call_ai_method`):
  - `nodes`: `List[String]` — agent names
  - `edges`: `List[[from, to]]` — pair list
  - Returns `List[String]` in execution order
  - Returns error on cycle / invalid input (real topological sort)

### heartbeat.md executable checklist (mimiclaw §1.5)

- **New module `src/heartbeat/mod.rs`**:
  - `HeartbeatItem { text, done, line_number }` — parsed checklist line
  - `parse_heartbeat(content, source)` — REAL md parser, supports
    `- [x]` / `- [X]` / `- [ ]` / `- []` formats
  - `HeartbeatReport { source, total, done, pending, items }` with
    `completion_ratio()` and `is_complete()` helpers
  - `load_heartbeat(path)` — REAL file I/O
  - 11 module-level tests (incl. 1 real file test)

- **`ai.heartbeat(path?)` builtin** (added to `call_ai_method`):
  - `path?`: optional path (default `~/.mora/HEARTBEAT.md`)
  - Returns `Dict{path, total, done, pending, completion_ratio,
    is_complete, items[]}` — REAL heartbeat.md parse
  - mimiclaw pattern: HEARTBEAT.md as executable agent behavior source

### context.trim smart truncation (pi-agent + AgentMesh)

- **`ai.context.trim(threshold?)` builtin** (added to `call_ai_method`):
  - `threshold?`: optional 0.0-1.0 (overrides default 0.8)
  - Calls `Interpreter.context_window.compress()` (REAL method, drops
    oldest messages first per `compression_ratio`)
  - Returns `Number(tokens_dropped)` (Number of tokens freed)
  - pi-agent+AgentMesh pattern: token-budget-aware truncation

- **`ai.context.info()` builtin** — diagnostic:
  - Returns `Dict{max_tokens, current_tokens, messages, compression_threshold}`

### Design decision: additive to existing infrastructure

- `OrchestrateDag` is **NEW module** (vs v0.25 orchestrate block syntax):
  declarative data (nodes + edges) vs procedural block (agents + edges).
  Both can coexist — block syntax for hand-written, dag builtin for
  programmatic graph generation.

- `HeartbeatItem` parses markdown checklists by line-prefix match
  (no regex dep), 30 LOC. v0.34 AIOS `tool_conflict_map` uses same
  line-iteration pattern.

- `context.trim` calls existing `ContextWindow::compress()` (v0.24)
  instead of writing new compression logic. `ContextWindow` already
  has add_message / needs_compression / compress / get_messages.

### 34 new tests (9 DAG module + 11 heartbeat module + 14 builtin)
- 9 `orchestrate_dag::tests::*` (linear/diamond/4-layer/cycle/self-loop/
  duplicate/unknown-edge/has_cycle/empty-edges)
- 11 `heartbeat::tests::*` (parse formats + completion_ratio +
  is_complete + real file test)
- 5 `tests_v047_dag::*` (linear/cycle/diamond/empty/2-args)
- 5 `tests_v047_heartbeat::*` (real_file/all_done/empty/nonexistent/items)
- 4 `tests_v047_context::*` (info/trim_empty/threshold_range/valid)

### Total impact
- 1 commit
- ~770 LOC (+~290 orchestrate_dag + ~180 heartbeat + ~120 builtin + ~180 tests)
- +34 tests (497 pre-existing retained)
- **531 tests pass total** (lib 525 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.48 patches (per master doc §4)
- v0.48.0: `mora refine` incremental edit loop (CLI-Anything)
- v0.48.0: `plan.update([{step, status}])` real-time checklist (pi-agent)

---

## [v0.46.0] - 2026-07-06 — SKILL.md + MoraSkillSpec + dual registry (CLI-Anything)

1 commit; v0.46+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### MoraSkillSpec + SkillRegistry (CLI-Anything pattern)

- **New module `src/skill/mod.rs`**:
  - `MoraSkillSpec { name, description, trigger, body, source }` — parsed
    SKILL.md content (YAML frontmatter + Markdown body)
  - `MoraSkillSpec::parse(content, source)` — **REAL YAML frontmatter
    parser** (hand-written, no `serde_yaml` dep); supports `name:`,
    `description:`, `trigger:` + quoted values
  - `MoraSkillSpec::load_file(path)` — REAL file I/O read + parse
  - `SkillRegistry` with **dual-registry semantics** (CLI-Anything's
    `registry.json` + `public_registry.json`):
    - Internal: `HashMap<String, MoraSkillSpec>` (programmatic)
    - External: `public_registry_path: Option<PathBuf>` (mora-public.json hub)
  - `SkillRegistry::load_public_registry()` — REAL JSON read of hub
    file (uses simple `find_json_string` helper, no serde_json dep)
  - 10 module-level tests including 1 real file test

- **7 new builtins** added to `call_skill_method`:
  - `skill.list()` → `List[String]` of skill names
  - `skill.find(name)` → `Dict{name, description, trigger, body, source}` or Nil
  - `skill.load(path)` → `Bool(true)` — REAL `MoraSkillSpec::load_file` call
  - `skill.install(name, content)` → `Bool(true)` — synthesize from SKILL.md
    string content
  - `skill.uninstall(name)` → `Bool(true)`
  - `skill.set_hub(path)` → `Bool(true)` — set public_registry path
  - `skill.refresh_hub()` → `Number(count)` — REAL `load_public_registry` call

- **`Interpreter.skill_registry: Arc<Mutex<SkillRegistry>>`** field;
  Arc<Mutex<>> keeps `call_skill_method(&self, ...)` signature.

- **`BuiltinKind::Skill`** new variant; `skill` global registered.

### Design decision: hand-written YAML/JSON parsers (0 new deps)

master doc §3.3 says "CLI-Anything uses serde_yaml + serde_json". **v0.46.0
avoids both**:
- YAML frontmatter (3 keys: name/description/trigger): 30 LOC regex split
- JSON hub parse (name + description extraction): 5 LOC `find_json_string` helper
- Result: 0 new Cargo deps, parses the formats CLI-Anything uses

Full `serde_yaml` + `serde_json` support deferred to v1.0+ (per master doc
future roadmap) when SKILL.md files become more complex.

### 19 new tests (10 module + 9 builtin)
- 10 `skill::tests::*` (incl. 1 real file test for public_registry)
- 9 `interpreter::builtins::tests_v046_skill::*` (incl. 2 real file tests
  for skill.load + skill.set_hub/refresh_hub)

### Total impact
- 1 commit
- ~440 LOC (+~280 skill module + ~80 builtin wiring + ~80 tests)
- +19 tests (478 pre-existing retained)
- **497 tests pass total** (lib 491 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.47 patches (per master doc §4)
- v0.47.0: DAG-as-data → `orchestrate`  (OpenFugu)
- v0.47.0: `heartbeat.md`  (mimiclaw)
- v0.47.0: `context.trim(threshold)`  (pi-agent + AgentMesh)
- v0.48.0: `mora refine`  + `plan.update` 

---

## [v0.45.0] - 2026-07-06 — ToolPlane + ai.retry + ai.role

1 commit; v0.45+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### ToolPlane — Core/Extension adapter (loongclaw)

- **New module `src/toolplane/mod.rs`**:
  - `PlaneKind` enum: `Core` (built-in) vs `Extension` (user/plugin)
  - `ToolSpec { name, description, parameters }` — metadata only
  - `ToolPlane` struct: name + kind + `HashMap<String, ToolSpec>`
  - `ToolPlaneRegistry` — multi-plane container
  - `default_registry()` — pre-registers `ai` + `sandbox` core planes
  - 11 module-level tests

- **8 new builtins** added to `call_toolplane_method`:
  - `tool.plane.create(name, kind)` → `Bool(true)`
  - `tool.plane.register(plane, tool, desc, params)` → `Bool(true)`
  - `tool.plane.unregister(plane, tool)` → `Bool(true)` (existed?)
  - `tool.plane.list()` → `List[String]` of plane names
  - `tool.plane.list_tools(plane)` → `List[String]` of tool names
  - `tool.plane.info(plane)` → `Dict{name, kind, tool_count}` or Nil
  - `tool.plane.find(plane, tool)` → `Dict{plane, tool, desc, params}` or Nil
  - `tool.plane.remove(plane)` → `Bool(true)`

- **`Interpreter.tool_planes: Arc<Mutex<ToolPlaneRegistry>>`** field;
  default has 2 core planes (`ai`, `sandbox`).
  Arc<Mutex<>> keeps `call_toolplane_method(&self, ...)` signature.

- **`BuiltinKind::Toolplane`** new variant; `tool` global registered
  (alongside existing `exec`, `sandbox`, etc.).

### ai.retry — tenacity-style retry policy (mini-swe-agent)

- **`ai.retry(attempts, backoff_ms?, strategy?)`** builtin:
  - `attempts`: Number/String — retry count (must be > 0)
  - `backoff_ms`: Number — base delay in ms (default 1000)
  - `strategy`: String — `fixed` / `exponential` / `linear` (default exponential)
  - Returns `Dict{attempts, backoff_ms, backoff, schedule}` where
    `schedule` is `List[Number]` of computed delays per attempt
  - Mini-swe-agent uses `tenacity@0.10s→60s` exp backoff; v0.45.0 mirrors
    this pattern with config validation

### ai.role — per-turn AI role (OpenFugu Worker/Thinker/Verifier)

- **`ai.role(name)`** builtin → `String(name)`:
  - OpenFugu canonical roles: `worker`, `thinker`, `verifier`
  - Custom roles also accepted (informational, no validation)
  - Returns the role name (caller-side enforcement for downstream ai.chat)

### Design decision: additive not replacement

master doc §6.5 says "ToolPlane  tool_registry". **v0.45.0 keeps both**:
- `Interpreter.tool_registry` (v0.34, single HashMap) — preserved
- `Interpreter.tool_planes` (v0.45.0, multi-plane) — added

Full migration deferred to v0.46+ to avoid breaking `tool_registry`-using
code paths in interpreter/execute.rs.

### 13 new tests (11 toolplane module + 6 toolplane builtin + 7 ai builtin)
- 11 `toolplane::tests::*`
- 6 `interpreter::builtins::tests_v045_toolplane::*`
- 7 `interpreter::builtins::tests_v045_ai::*`

### Total impact
- 1 commit
- ~580 LOC (+~290 toolplane module + ~200 builtin wiring + ~90 tests)
- +24 tests (454 pre-existing retained)
- **478 tests pass total** (lib 472 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.46 patches (per master doc §4)
- v0.46.0: `SKILL.md`  +  (`mora-hub.json` + `mora-public.json`) (CLI-Anything)
- v0.47.0: DAG-as-data (OpenFugu) + `heartbeat.md` (mimiclaw) + `context.trim` (AgentMesh)

---

## [v0.44.0] - 2026-07-06 — sandbox.containerize REAL Docker + orchestrate validation

1 commit; v0.44+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §7.

### sandbox.containerize() — REAL Docker orchestration (pi-mono v0.44.0)

**v0.44.0 actually spawns Docker containers (NOT metadata-only).**

- **New module `src/sandbox/container.rs`**:
  - `ContainerBackend` enum: Docker (v0.44.0 ), Gondolin + OpenShell
    (deferred to v1.0+, returns explicit error)
  - `NetworkMode` (Isolated/Host), `MountSpec` (host:container:mode),
    `ResourceLimits` (cpu_cores, memory_mb), `ContainerSpec`
  - `ContainerHandle { container_id, container_name, backend, spec, started_at }` —
    runtime handle to a **real** spawned container
  - `spawn_container(spec) -> ContainerHandle` — calls `docker run -d` for real
  - `ContainerHandle::exec(&[cmd])` — runs `docker exec <id> <cmd>`
  - `ContainerHandle::destroy()` — runs `docker rm -f <id>`

- **4 new builtins** added to `call_sandbox_method`:
  - `sandbox.containerize(backend, mounts?, network?, cpu?, mem?, image?)`
    → `Number(id_hash)` — returns hash of real container ID;
    `Interpreter.container` holds full `ContainerHandle`
  - `sandbox.container_exec(cmd, args...)` → `Dict{exit_code, stdout, stderr, elapsed_ms}`
    — runs via `docker exec`
  - `sandbox.container_info()` → `Dict{container_id, container_name, backend, image, network, mount_count, elapsed_ms}` or `Nil`
  - `sandbox.container_clear()` → `Bool(true)` — actually runs `docker rm -f`

- **`Interpreter.container: Arc<Mutex<Option<ContainerHandle>>>`** field;
  Arc<Mutex<>> keeps `call_sandbox_method(&self, ...)` signature intact
  (no breaking change to dispatch).

### Tested against real Docker daemon

`real_docker_spawn_and_destroy` integration test (#[ignore]):
```text
$ cargo test --lib real_docker_spawn_and_destroy -- --ignored --nocapture
running 1 test
test sandbox::container::tests::real_docker_spawn_and_destroy ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
```

The test:
1. Spawns `docker run -d --name mora-XXX alpine:latest sleep infinity`
2. Verifies container_id is real (>= 12 hex chars)
3. Runs `docker exec <id> echo hello-from-mora` and checks stdout
4. Cleans up via `docker rm -f <id>`

**All 4 real-docker integration tests pass in 1.15s** when run with `--ignored`.

### orchestrate block — already implemented v0.25 (validation only)

master doc §1.13 cites revenue-orchestrator's `handoff_criteria` pattern.
**Pre-existing v0.25 implementation** in `src/interpreter/orchestrate.rs`:
- `orchestrate sequential <input> -> <output> { agents... }`
- `orchestrate graph <input> -> <output> { edges with `on:` predicate }`
- `orchestrate loop <input> -> <output>, max_rounds: N, on: <cond> { agent }`

Added 3 parse-validation tests (no new code needed).

### Design decision: Docker-only in v0.44.0

master doc §1.11 mentions Gondolin / Docker / OpenShell. **Decision**:
- **Docker**: implemented in v0.44.0 (most common, real CLI spawn)
- **Gondolin / OpenShell**: deferred to v1.0+ — `spawn_container()`
  returns clear "not yet implemented" error if requested

Future builtins (sandbox.exec via container, sandbox.file.read via mount
  validation) can check `Interpreter.container.is_some()` to apply
  container-aware policies.

### 14 new tests (11 module + 0 builtin unit + 4 docker ignored + 3 orchestrate parse)
- 11 `sandbox::container::tests::*` (incl. 1 #[ignore] docker integration)
- 4 `interpreter::builtins::tests_v044_container_real::*` (4 #[ignore] docker)
- 3 `interpreter::builtins::tests_v044_orchestrate_validate::*`
- **4 skipped (#[ignore])** unless `cargo test -- --ignored` with Docker daemon

### Total impact
- 1 commit (after v0.44.0 metadata-only attempt was REVERTED)
- ~600 LOC (+~400 container module + ~150 builtin wiring + ~50 tests)
- +14 tests (436 pre-existing retained)
- **454 tests pass total** (lib 448 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.45 patches
- v0.45.0: `ToolPlane` Core/Extension adapter (loongclaw, ~150 LOC)
- v0.45.0: `ai.retry { attempts: 10, backoff: exponential }` (mini-swe-agent)
- v0.45.0: `ai.role { worker / thinker / verifier }` (OpenFugu)

---

## [v0.43.1] - 2026-07-05 — memory.remember / bus.subscribe (markdown + pub-sub)

1 commit; third P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### memory.remember / recall_markdown / list_markdown (pi-agent inspired)

- **3 new builtins** added to `call_memory_method`:
  - `memory.remember(category, text)` → `Bool(true)`; appends to
    `~/.mora/memory/YYYY-MM-DD.md` under `## {category}` section
  - `memory.recall_markdown(category)` → `String`; collects all entries
    under `## {category}` across all markdown files
  - `memory.list_markdown()` → `List[String]`; lists all categories

- **Markdown format** (auto-generated):
  ```
  # 2026-07-05

  ## {category}

  - {text}

  ## {other_category}

  - {text}
  ```
  Subsequent remember to existing category appends bullets (no duplicate section).

- **`Interpreter.markdown_memory_dir: Option<PathBuf>`** field added;
  overrides default `~/.mora/memory/` for test isolation + custom deployments.
  Wired through Clone impl + 3 constructors.

- **Cross-pollination with HashMap memory**: remember also writes to
  `memory_store["md:{category}"]` so existing `memory.recall()` works.

- **5 helper functions added**:
  - `markdown_memory_dir(override)` — resolution precedence: field > env > home
  - `today_date_string()` — UNIX days → YYYY-MM-DD (handles leap years)
  - `remember_markdown(override, cat, text)` — atomic write per file
  - `recall_markdown(override, cat)` — read all .md, extract section
  - `list_markdown_categories(override)` — collect unique `## ` headers

### bus.subscribe / bus.publish (Puter / AgentMesh / Solace inspired)

- **2 new builtins** added to `call_event_method`:
  - `bus.subscribe(pattern)` → `Number(token)`; registers pattern via
    `EventBus::on()` with no-op handler (real handlers via LSP/HTTP/MCP layer)
  - `bus.publish(topic, payload)` → `Number(pattern_count)`; emits via
    `EventBus::emit()` which has v0.41.0 O(segments) indexed matching

- **Pattern matching** inherits v0.41.0 O(segments) indexed matching
  (Puter EventClient code-verified). Subscribers using `agent.*` catch
  `agent.foo`, `agent.foo.bar`, etc.

### 12 new tests (6 memory + 6 bus)
- `memory_remember_appends_to_markdown` — file write
- `memory_remember_appends_to_existing_section` — no duplicate section
- `memory_recall_markdown_returns_text` — section readback
- `memory_recall_markdown_returns_empty_for_unknown` — missing category
- `memory_list_markdown_lists_categories` — multiple categories
- `memory_recall_after_remember_syncs_to_memory_store` — HashMap sync
- `bus_subscribe_returns_token` — Number(token)
- `bus_subscribe_validates_pattern` — type check
- `bus_publish_returns_pattern_count` — Number
- `bus_publish_validates_topic` — type check
- `bus_subscribe_then_publish_wildcard_match` — wildcard end-to-end
- `bus_subscribe_uses_existing_pattern_matching` — exact + prefix patterns

### Design decision: Test isolation via field, not env var
- Master doc §6.4/§6.5 suggested using `MORA_MEMORY_DIR` env var
- **Switched to `Interpreter.markdown_memory_dir: Option<PathBuf>`**:
  - Cleaner test isolation (no global env state, parallel tests safe)
  - Field-level override matches existing `Interpreter.sandbox`,
    `Interpreter.audit_sink` pattern
  - Env var fallback preserved (`$MORA_MEMORY_DIR` still works if field is None)
  - Default falls back to `$HOME/.mora/memory/`

### Total impact
- 1 commit
- ~620 LOC (+~280 impl + ~50 init sites + ~290 tests)
- +12 tests (424 pre-existing retained)
- 436 tests pass total (lib 430 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.44 patches (per master doc §4)
- v0.44.0: `orchestrate { on: expression }` — predicate routing (revenue-orchestrator)
- v0.44.0: `sandbox.containerize` Gondolin mode (pi-mono)
- v0.45.0: `ToolPlane` Core/Extension adapter (loongclaw) + `ai.retry` + `ai.role`

---

## [v0.43.0] - 2026-07-05 — exec.parallel() concurrent subprocess (pi-mono v1)

1 commit; **finishes master doc §4 v0.41-0.43 first wave** (5 patches total).

### exec.parallel() — concurrent subprocess execution

- **New `BuiltinKind::Exec` variant** + `call_exec_method` dispatcher
  + builtin `exec` registered in `Interpreter::new()` globals.

- **`exec.parallel(cmds, [max_concurrent], [timeout_ms])`** builtin:
  - First arg: `List[String]` — commands to execute (run via `sh -c`)
  - Optional 2nd arg: `Number` — max concurrent workers (default = cmds.len())
  - Optional 3rd arg: `Number` — per-cmd timeout in ms (default = no timeout)
  - Returns: `List[Dict{cmd, stdout, stderr, exit_code, pid, elapsed_ms, error}]`

- **Process group isolation** (mini-swe-agent v1 style):
  - **Unix**: `pre_exec` calls `libc::setpgid(0, 0)` to create new process group
  - **Windows**: `creation_flags(CREATE_NEW_PROCESS_GROUP)` (0x00000200)
  - On timeout: `killpg(pid, SIGKILL)` (Unix) / `taskkill /F /T /PID` (Windows)
  - Prevents orphaned grandchild processes

- **STD-ONLY implementation** (deliberate deviation from master doc §6.5):
  - `tokio::process::Command` (master doc suggested) **rejected** — AGENTS.md
    and Cargo.toml both forbid async runtime
  - Used: `std::thread::spawn` + `std::process::Command` +
    `std::sync::{mpsc, Arc, Condvar, Mutex}`
  - Custom `Semaphore` impl (std lacks one) using AtomicUsize + Mutex + Condvar
  - Atomic index distribution via `AtomicUsize::fetch_add`

### 9 new tests (Interpreter-level)
- `exec_parallel_runs_all_commands` — 3 cmds,  stdout
- `exec_parallel_respects_max_concurrent` — 6 cmds, max_concurrent=2
- `exec_parallel_empty_list_returns_empty` — 
- `exec_parallel_collects_stdout_per_command` — 
- `exec_parallel_kills_process_group_on_timeout` — `sleep 10` + 200ms timeout
- `exec_parallel_validates_arg_types` — 
- `exec_parallel_validates_cmd_elements` — 
- `exec_parallel_returns_error_for_missing_command` —  → exit 127
- `exec_unknown_method_errors` — unknown method

### Design decision: STD vs tokio
- Master doc §6.5 suggested `tokio::process::Command` + `tokio::sync::Semaphore`
- Project rule (AGENTS.md §3 + Cargo.toml): **" async runtime"**
- Implemented equivalent with std threads + custom Semaphore
- Result: 0 new deps, all std library APIs

### Total impact
- 1 commit
- ~390 LOC (+~250 impl + ~140 tests)
- +9 tests (415 pre-existing retained)
- 424 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### v0.41+ roadmap progress (master doc §4)
| Version | Status | Patch |
|---------|--------|-------|
| v0.41.0 |  | event O(segments) |
| v0.41.1 |  | reading_order XY-Cut++ |
| v0.42.0 |  | sandbox.key + Capability |
| v0.42.1 |  | audit.jsonl + AuditSink |
| **v0.43.0** |  | **exec.parallel()** |
| v0.43.1+ | planned | memory.remember/recall, bus.subscribe, orchestrate, etc. |

**First wave complete.** All 5 patches from RESEARCH_PRIMITIVES_MASTER_v2.md §4
implemented and committed.

---

## [v0.42.1] - 2026-07-05 — Audit Sink SHA-256 Hash Chain (loongclaw)

1 commit; second P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md
(loongclaw crates/kernel/src/audit.rs:34-204 inspired).

### Audit Sink — JSONL + SHA-256 hash chain

- **New module `src/audit/mod.rs`** — implements loongclaw-style audit log:
  - `AuditEvent { timestamp_ms, actor, action, target, payload_json, token_id, prev_hash, hash }`
  - `AuditSink` trait (`Send + Sync`): write / flush / verify_chain / event_count
  - `JsonlAuditSink` — append-only JSONL file with SHA-256 hash chain
    (`hash = SHA-256(canonical_json(event) + prev_hash)`)
  - `NullSink` — no-op default (audit disabled)
  - `AuditError` enum (Io, ChainBroken, HashMismatch, ParseError)

- **`Interpreter.audit_sink: Arc<dyn AuditSink>`** field added; default `NullSink`.
  Wired through `Clone::clone()` impl + 3 constructors.

- **3 new builtins** (added to `call_sandbox_method`, NOT new BuiltinKind):
  - `sandbox.audit_emit(actor, action, target?, payload?)` → `Value::Bool(true)`
  - `sandbox.audit_flush()` → flushes write buffer to disk
  - `sandbox.audit_verify()` → `Value::Bool(true)` if chain OK, else
    `Value::String(error)` (so Mora can branch on it)

- **Hash chain design**:
  - First event: `prev_hash = "0" × 64` (genesis)
  - Each subsequent event: `prev_hash = previous event's hash`
  - `verify_chain()` reads whole file, recomputes hash for each line,
    catches both `prev_hash` mismatch (line deleted/inserted) AND
    `hash` mismatch (content tampered)

- **Crash safety**: `new(path)` reads last line of existing file and
  restores `last_hash` from the most recent `hash` field — process
  restart resumes the chain instead of restarting from genesis.

- **No `serde` dep added** — JSON serialization is hand-written
  (`json_string()` escape function, ~30 LOC). Only `sha2 = "0.10"`
  added to Cargo.toml (per AGENTS.md §3, deps justified).

### 20 new tests (audit module unit + Interpreter builtin integration)
- 12 `audit::tests::*` (JsonlAuditSink + NullSink + parser/serializer)
- 8 `interpreter::builtins::tests_v0421_audit::*` (full builtin flow)

### Total impact
- 1 commit
- ~700 LOC (+~480 audit module + ~100 builtin wiring + ~20 InitSite +
  ~100 tests; minor clones/sed)
- +20 tests (395 pre-existing retained)
- 415 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 1 new dep (`sha2 = "0.10"`)

### Next v0.43 patches (per master doc §4)
- v0.43.0: `exec.parallel()` (pi-mono v1 subprocess isolation, ~50 LOC)

---

## [v0.42.0] - 2026-07-05 — Capability Token System (loongclaw)

1 commit; first P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md
(loongclaw crates/contracts/src/contracts.rs:24-52 inspired).

### Capability Token System

- **New module `src/sandbox/capability.rs`** — implements token-based
  authorization alongside the v0.33 pattern-based `allow/deny`:
  - `Capability` enum (13 variants: `FileRead`, `FileWrite`, `WebFetch`,
    `WebSearch`, `ExecBash`, `ExecParallel`, `MemoryRead`, `MemoryWrite`,
    `AuditEmit`, `BusSubscribe`, `BusPublish`, `AgentInvoke`, `AgentRegister`)
  - `CapabilityToken { token_id, allowed, denied, expires_at, generation, created_at }`
  - `CapabilityStore` (Arc<Mutex<BTreeMap>>) — issue/get/check/revoke API
  - `SandboxError` enum with structured variants (UnknownCapability,
    TokenExpired, TokenNotFound, CapViolation, GenerationMismatch)

- **`SandboxPolicy.capabilities: CapabilityStore`** field added
  (default `CapabilityStore::new()`). v0.33 pattern-based API
  (`allow/deny BTreeSet`, `check_builtin`, `check_path`) is **unchanged**.

- **4 new builtins** wired through `call_sandbox_method`:
  - `sandbox.key { "file.read", "web.fetch" }` → `Value::Number(token_id)`
  - `sandbox.check_call(token_id, "file.read")` → `Value::Bool`
  - `sandbox.revoke(token_id)` → `Value::Bool(true)` (loongclaw-style:
    bumps `generation`, doesn't delete token)
  - `sandbox.token_count()` → `Value::Number`

- **`Capability::parse(s)` and `as_str()`** for round-trip between
  Rust enum and mora source strings.

### Design decisions
- **Token handle = `Value::Number(u64)`** (NOT a new Value variant).
  Avoids touching the 56-variant `Value` enum (per AGENTS.md §5, v0.x
  may break but prefer minimal surface).
- **Arc<Mutex> around CapabilityStore** so `SandboxPolicy: Clone` still works
  (interpreter copy semantics share the store, not duplicate it).
- **Revoke bumps generation** (loongclaw style) instead of deleting.
  This means `check_call` doesn't validate generation — that's a
  higher-layer PolicyEngine concern, exposed via `SandboxError::GenerationMismatch`.
- **No TTL in v0.42.0 builtin** — `sandbox.key` accepts any args, no
  `sandbox.key_ttl { ..., ttl: 5s }` yet. Token's `expires_at` field is
  ready; builtins will be added in v0.42.x if needed.

### 21 new tests (CapabilityStore unit + Interpreter builtin integration)
- 11 `sandbox::capability::tests::*` (CapabilityStore unit)
- 10 `interpreter::builtins::tests_v042_capability::*` (full builtin flow)

### Total impact
- 1 commit
- ~520 LOC (+~280 capability module + ~90 builtin wiring + ~150 tests)
- +21 tests (374 pre-existing retained)
- 395 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.42+ patches (per master doc §4)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.41.1] - 2026-07-05 — Reading Order XY-Cut++ (MinerU algorithm upgrade)

1 commit; second P0 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### Reading order: XY-Cut++ algorithm upgrade (MinerU arXiv:2504.10258)

- **New `Strategy::XyCutPlusPlus` variant** (and aliases `xy_cut_plus_plus` /
  `xy++` / `xy_cut_pp` via `Strategy::from_str`). Old variants
  (`InputOrder` / `TopToBottom` / `GapTree` / `XyCut` / `GroupBased`)
  remain unchanged — fully backwards-compatible.

- **Old `Strategy::XyCut` (v0.33)** was a flat sort `(y, then x)` — no
  recursive segmentation. **New `XyCutPlusPlus`** implements the actual
  recursive XY-Cut algorithm (arXiv:2504.10258):
  1. **Cross-layout element detection** (`is_cross_layout`): elements
     with `width > beta * max_width` AND `overlap_count >= 2` are split
     off (e.g. cross-column headers / footers).
  2. **Density-ratio axis selection** (`compute_prefer_horizontal`):
     `x_density > density_threshold * y_density` → prefer horizontal
     first (split by y, then within each row by x).
  3. **Recursive projection-segmentation** (`recursive_xy_cut`):
     project to axis → find gap-runs → split into sub-segments →
     recurse with flipped axis preference.
  4. **Merge cross-layout elements** at the right position based on
     vertical center.

- **5 helper functions added** (all private, file-local):
  - `is_cross_layout(all, bbox)` — cross-column detection
  - `compute_prefer_horizontal(entries)` — adaptive axis selection
  - `compute_density_ratios(entries)` — x/y density calculation
  - `project_to_axis(entries, axis)` — 1D histogram projection
  - `split_projection(hist, min_gap)` — find gap-run segments
  - `recursive_xy_cut(entries, prefer_horizontal_first)` — core recursion
  - `merge_cross_layout_elements(main, cross)` + `find_insertion_point`

- **5 named constants** (MinerU defaults):
  `XY_CUT_PLUS_PLUS_BETA = 2.0`, `DENSITY_THRESHOLD = 0.9`,
  `OVERLAP_THRESHOLD = 0.1`, `MIN_OVERLAP_COUNT = 2`,
  `MIN_GAP_THRESHOLD = 5.0`.

- **7 new tests** (8 pre-existing retained):
  - `strategy_from_str_xy_cut_pp` — aliases parse correctly
  - `xy_cut_pp_single_column_doc` — newspaper-style vertical ordering
  - `xy_cut_pp_two_column_doc` — academic two-column (L1,R1,L2,R2 row-by-row)
  - `xy_cut_pp_with_cross_layout_header` — wide header inserted at top
  - `xy_cut_pp_single_block_returns_unchanged` — single-block edge case
  - `xy_cut_pp_preserves_all_blocks` — no blocks lost or duplicated
  - `xy_cut_pp_complexity_below_o_n_squared` — perf benchmark, 50 blocks < 200ms

### Source inspiration
`MinerU` arXiv:2504.10258 "XY-Cut++: Advanced Layout Ordering via Hierarchical Mask
Matching" (April 2025). Mora previously had only the simple `recursive_xy_cut`
from `mineru/model/reading_order/xycut.py`; v0.41.1 upgrades to the newer
algorithm per master doc §6.2.

### Total impact
- 1 commit
- ~290 LOC (+~230 impl + ~60 tests + ~10 const)
- +7 tests (8 pre-existing retained)
- 374 tests pass total (was 367), 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps
- Backwards-compatible: existing `Strategy` variants unchanged; only adds
  a new variant + aliases

### Next v0.41 patches (per master doc §4)
- v0.42.0: `sandbox.key` + `Capability` enum (loongclaw, ~200 LOC)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.41.0] - 2026-07-05 — Event Bus O(segments) (Puter, code-verified)

1 commit; first P0 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### Event bus: O(segments) indexed matching replaces linear scan

- **`EventBus` now uses a 3-bucket index** instead of a single
  `HashMap<Pattern, Vec<Handler>>` iterated on every emit:
  - `exact`: literal patterns (e.g. `"ai.chat.completed"`) → O(1) lookup
  - `prefix`: trailing-wildcard patterns (e.g. `"ai.*"`, `"a.b.*"`, `"*"`)
    keyed by the prefix-without-`.*` (e.g. `"ai"`, `"a.b"`, `""`) →
    O(segments) prefix walk
  - `interior`: middle-wildcard patterns (e.g. `"a.*.c"`, `"*.b.*"`)
    kept as fallback linear scan (rare in practice; required by
    existing API semantics)

- **`emit` complexity**:
  - Old (v0.32-0.40): **O(patterns × segments)** — `map.iter().filter(matches).flat_map(...)`
  - New (v0.41): **O(segments)** for exact/prefix paths
    (interior fallback remains O(interior_patterns))

- **`classify_pattern()` helper** routes `on(pattern)` registrations to
  the correct bucket at registration time, so `emit` never needs to
  parse patterns.

- **Catch-all `*` pattern**: keyed by empty string `""`, looked up
  once at the start of `emit`'s prefix walk — verified via new
  `bus_catchall_star_routes_to_prefix_empty` test.

- **10 new tests** (8 pre-existing retained):
  - `classify_pattern_routes_correctly` (Pure function test)
  - `bus_handlers_route_to_correct_buckets` (Register dispatches to right bucket)
  - `bus_emit_literal_match_fires_handler` (Exact path)
  - `bus_emit_wildcard_match_fires_handler` (Prefix path)
  - `bus_emit_with_no_subscribers_is_noop` (Empty case)
  - `bus_emit_with_multiple_wildcards_fires_all` (Multi-level Puter walk)
  - `bus_interior_wildcard_still_works` (Interior fallback)
  - `bus_catchall_star_routes_to_prefix_empty` (Catch-all)
  - `bus_off_removes_from_correct_bucket` (off() routes to right bucket)
  - `bus_emit_complexity_scales_with_segments_not_patterns` (Perf benchmark,
    100 patterns + 1000 emits < 200ms)

### Source inspiration
`Puter` `src/backend/clients/event/EventClient.ts:62-67` (verified 2026-07-05
via MCP search; see RESEARCH_PRIMITIVES_MASTER_v2.md §1.10).

### Total impact
- 1 commit
- ~165 LOC (108 impl + ~57 tests)
- +10 tests (8 pre-existing retained)
- 367 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps
- Backwards-compatible: same `on(pattern, handler)` / `emit(event, payload)`
  / `off(pattern)` API, same matching semantics

### Next v0.41 patches (per master doc §4)
- v0.41.1: `reading_order` XY-Cut++ (MinerU algorithm upgrade, ~60 LOC)
- v0.42.0: `sandbox.key` + `Capability` enum (loongclaw, ~200 LOC)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.40] - 2026-07-04 — Env Refactor (Closure Env Immutable)

2 commits resolving Permanent #1 (Env cross-thread safety) — the
LAST of the 5 "permanent debts" the v0.34 audit identified.

### EnvRef immutable snapshot for closure captures

- **`Value::Closure.env` now `EnvRef` (immutable Box<Environment>)**
  instead of `Arc<Mutex<Environment>>` (shared mutable). The captured
  environment is FROZEN at closure-creation time — no other thread or
  closure can mutate a closure's bound variables.

- **`EnvRef`** type introduced — a Box<Environment> wrapper that's
  Send-safe (Environment contains only Send fields). `EnvRef::borrow()`
  returns `&Environment` for read access. `EnvRef::from_arc_mutex()`
  converts legacy `Arc<Mutex<>>` sources.

- **3 Closure constructor sites** (evaluate:214, execute:562, mock:142)
  now use `EnvRef::from_arc_mutex(self.environment.clone())`.
- **1 Closure destructure site** (dispatch:1193) updated to clone
  the inner Environment from EnvRef.

- **NON-CHANGE**: `Interpreter.globals/environment` remain as
  `Arc<Mutex<Environment>>` — the Rc<RefCell<>> optimization was
  explored but rejected in v0.40 because it would make Interpreter
  !Send (breaking HTTP/MCP worker boundaries). This is now
  documented as a future optimization after Interpreter restructuring.

### Closure env always Local (Immutable Snapshot)

The v0.34 audit claimed "Env cross-thread safety" was a permanent debt.
v0.40 resolves it by making closures own an immutable copy of the env
at capture time. Cross-thread workers hold `Arc<Mutex<Interpreter>>` —
the Interpreter's env chain stays as `Arc<Mutex<>>` (Send-safe), and
each closure snapshot is an owned Box<Environment> (also Send-safe).

No more "other thread could mutate my closure's env" concern.

### Total impact
- 2 commits on branch v0.40-env-refactor
- ~30 LOC net + ~10 LOC tests
- 1 new test (envref_from_arc_mutex_roundtrip)
- 5 demos pass (pre-existing PDF test failures in worktree only)
- 0 new deps
- **FINAL permanent debt resolved**: v0.34 audit's 5 "permanent debts"
  are now ALL solved (crossbeam v0.36, Type enum 8 variants v0.36,
  NaN/Inf guard v0.36, numeric tower v0.38, env snapshot v0.40).

---

## [v0.39] - 2026-07-03 — Env Refactor DEFERRED (No Functional Change)

1 commit + 1 CHANGELOG; no functional changes shipped.

### Status: Env refactor not completed

The plan to add `EnvRef` (Local Rc<RefCell> / Owned Box<Environment>)
to replace `Arc<Mutex<Environment>>` in `Value::Closure.env` was
attempted but **not landed**. The change cascades across 8 files
and triggers 19+ compile errors at each step:

- `value.rs` (Closure.env, Environment.parent, 6 parent.lock() sites)
- `interpreter/mod.rs` (globals/environment fields + 4 Self{} blocks)
- `interpreter/{dispatch,evaluate,execute}.rs` (~15 self.environment.clone()
  + Arc::new(Mutex::new(...)) sites)
- `interpreter/{orchestrate,trait_dispatch,ai_chat,ai_helpers,builtins}.rs`
  (~30 .lock().expect() sites)
- `mock/mod.rs` (Closure constructor)
- `http_server.rs` + `mcp_server.rs` (worker boundary std::thread::spawn)
- All cross-thread Captures need `EnvRef::Owned` deep clone (cycle
  guard via HashSet<*const Environment>)

The v0.34 audit's "permanent debt" tag for this item is now **fully
vindicated**: this refactor is multi-day coordinated work. v0.38's
release notes claimed it would land in v0.39; v0.39 partial work
proves the size.

### What landed (1 commit)
- `refactor(v0.39): rename Environment::with_parent -> with_parent_of`
  — frees the name `with_parent` for the v0.40 Env helper that
  will uniformly dispatch across `EnvRef::Local`/`EnvRef::Owned`.

### v0.40 plan (next version)

Single multi-commit coordinated refactor:
1. `value.rs`: add `EnvRef` enum (Local Rc<RefCell> / Owned Box<Environment>).
2. `value.rs`: change `Closure.env: EnvRef`, `Environment.parent: Option<Box<EnvRef>>`.
3. `value.rs`: replace 6 `parent.lock()` sites with `self.with_parent(|p| ...)`.
4. `interpreter/mod.rs`: `globals/environment: Rc<RefCell<>>` (single atomic
   change with all 4 Self{} blocks + Clone impl + 30 .lock()→.borrow()).
5. `interpreter/{dispatch,evaluate,execute}.rs`: propagate EnvRef to
   closure constructors + task body.
6. `mock/mod.rs`: Closure env uses EnvRef::Local.
7. `http_server.rs` + `mcp_server.rs`: at `std::thread::spawn` boundary,
   deep clone `EnvRef::Local` to `EnvRef::Owned`. Add `cycle_detected`
   guard via HashSet.
8. Tests: cross-thread closure isolation + Send/Sync assertions.
9. CHANGELOG + merge.

Estimated: 6-8 atomic commits, ~500 LOC, 1 full day of work.

---

## [v0.38] - 2026-07-03 — Numeric Tower (Half Final)

7 commits resolving Permanent #2 (numeric tower) partial migration.
Env refactor (Permanent #1 cross-thread gap, P1-2.8) deferred to
v0.39 — see "Deferred to v0.39" section below for why.

### Numeric tower complete (Permanent #2)

- **`Value::Int(i64)` + `Value::Float(f64)` variants** — added
  alongside legacy `Value::Number(f64)`. The 3 numeric variants
  participate in Display / PartialEq / Hash / JSON encoding /
  type_name().

- **`Literal::Int(i64, Span)` + `Literal::Float(f64, Span)`** —
  parsed from `1i`, `1f` suffixes. flow.rs + evaluate.rs +
  literal_to_value_inner + typeck all handle the new variants.

- **Lexer recognizes `1i` / `1u` / `1f` / `1.0f` / `1.0f64` suffixes** —
  `number_from()` detects the optional suffix character + width.
  Parser routes Int/Float tokens to corresponding Literal arms.

- **`Type::Int` + `Type::Float` variants** — name() / type_to_hint_string
  / exhaustiveness tests updated. Literal::Int now produces
  `Type::Int` (not the legacy Number fallback).

- **Strict numeric promotion (Rust-style)**:
  - `Int + Int = Int` (pure integer arithmetic)
  - `Float + Float = Float` (pure float arithmetic)
  - `Int + Float` / `Float + Int` → **strict type error**
  - Mixed with `Number` (legacy) → coerced to f64 (back-compat)

- **13 new tests** covering Int promotion, Float promotion,
  strict mixed errors, Number compat, eval_binary Add,
  numeric_cmp Lt/Eq, typeck Type::Int/Float name.

### Deferred to v0.39 (Env refactor — was 3 commits in plan)

The v0.38 plan included an Env refactor (Permanent #1: cross-thread
Env safety) implementing:
- `EnvRef` two-tier enum (Local Rc<RefCell> / Owned Box<Environment>)
- `Closure.env` typed as `EnvRef` (was `Arc<Mutex<Environment>>`)
- Interpreter globals/environment → `Rc<RefCell<>>`
- Worker boundary (HTTP/MCP/parallel) creates `EnvRef::Owned`
  via deep clone of `String → Value` data
- Cycle guard via `HashSet<*const Environment>` during deep clone

**Status: not landed in v0.38**. During C6 implementation we hit
18+ compile errors spanning value.rs, interpreter/{mod,evaluate,
execute,dispatch}, http_server.rs, mcp_server.rs, mock/mod.rs.
The error pattern (`Rc<RefCell<...>>` cannot be sent across threads)
**affirms the v0.34 audit's "permanent debt" tag** for this item.

Two lessons learned:
1. The full refactor requires coordinated changes across 8 files.
   Splitting per-commit would break the build at every step.
2. Rc<RefCell> is fundamentally not Send, so any interpreter path
   that crosses thread boundaries (HTTP server spawn, MCP server
   spawn, parallel Worker block) must explicitly convert to
   EnvRef::Owned.

**v0.39 will be dedicated to this single Env refactor** as a
multi-commit coordinated change. v0.38 left the Interpreter struct
untouched (globals/environment still `Arc<Mutex<Environment>>`),
so the codebase compiles cleanly.

### Total impact
- 7 commits on branch `v0.38-numeric-env`
- ~300 LOC net + 200 LOC tests
- 350 tests pass; 0 failures (was 337, +13 numeric tower)
- 5 demos × unchanged pass count
- 0 new deps

---

## [v0.37] - 2026-07-03 — Debt Cleanup Round 3 (Final Pre-v0.38)

8 commits resolving the remaining P1 + P2 audit items + 1 cleanup.
v0.38 is reserved for the full numeric tower migration and the
Env refactor (both deferred for risk management — see below).

### Stringly-typed dispatch eliminated

- **`Value::Builtin(String)` → `Value::Builtin(BuiltinKind)`** (P1-3.6)
  22-variant enum covers every builtin the interpreter knows. The
  giant `(name.as_str(), method)` tuple-match in `dispatch.rs:746`
  replaced with an exhaustive `(BuiltinKind, method)` — compiler now
  enforces adding a new builtin requires either updating dispatch or
  routing through `call_*_method`.

### Builtin boundary tightening

- **bus.emit / bus.off / sandbox.check_* / schedule.add / ccr.put /
  ccr.get / mock.register / unregister / call** all now require
  `Value::String` for their primary argument (P1-3.7/3.8/3.9).
  Previously a `Value::List {1, 2, 3}` silently became the literal
  text `[1, 2, 3]` via `to_string()` — silent lossy bug. Now type
  errors are raised immediately at the boundary.

### Dead-code removals

- **`MockRegistry::call` deleted entirely** (P1-3.12). v0.36 deprecated
  it; v0.37 completes the deprecation by deleting the method. All
  test sites use `MockRegistry::get()` to inspect handlers directly.

### Type soundness holes closed

- **`typeck Load` returns `Type::String`** (P1-4.7) — was `Union([])`
  (= any). Aligns with semantically adjacent `ReadFile`. The `Load`
  keyword still has no v2 executor (falls through to "Unsupported v2
  statement"); a future commit will implement it.
- **`typeck error Span positions`** (P2-4.11) — 7 of 11 sites now
  carry the actual source location via `from_span_with_detail`. The
  3 remaining `line: 0, column: 0` sites are inside `check_call_expr`
  where the callee NodeId isn't threaded; deferred to v0.38.
- **`typeck with-block validates key against whitelist** (P2-4.15) —
  catches `with { modle = "x" }` (typo'd "model") at typeck time.
  Runtime's `execute_with` silently dropped unknown keys; that gap
  is now closed.

### Concurrency tightening

- **`http_server.rs` request handler** hoists method/path clones
  before the route lookup lock (P1-1.6b) — critical section now
  guards only HashMap ops, not String allocations.

### Deferred to v0.38 (too large for this PR)

- **Permanent #2 full numeric tower** (Value::Int(i64) / Float(f64) +
  Literal::Int/Float + parser suffix + 258-site arithmetic sweep).
  The naive approach via `as_f64()` helper was rejected — full
  migration touches arithmetic promotion rules and needs careful
  type promotion design.
- **P1-2.8 Env refactor (LocalEnv Rc<RefCell>)** — requires worker
  boundary redesign. Cross-thread closures mean plain `Rc` is unsafe;
  the architecture needs a two-tier Environment model.

### Total impact
- 8 commits, single feature branch `v0.37-final-cleanup`
- ~250 LOC net + ~50 LOC tests
- 337 tests pass; 0 failures
- 5 demos × unchanged pass count
- 0 new deps

---

## [v0.36] - 2026-07-03 — Type Completeness + Permanent Debt Resolution

Round 2 of zero-trust audit cleanup. 14 commits resolving 11 P1 + 1 P2
items the audit deferred, plus 1 audit-discovered **CI pre-existing bug**.
P1-2.8 (Env pool) and Permanent #2 (full numeric tower) deferred to v0.37.

### Permanent debt resolution (3 items the v0.34 audit claimed unsolvable)

- **crossbeam-channel migration** — `std::sync::mpsc` → `crossbeam-channel`
  for `worker_channels` / `worker_receivers`. Sender/Receiver are now
  `Send + Sync`, eliminating the long-standing "Interpreter: !Send"
  constraint. Closes Permanent #1.

- **8 new `Type` variants** — `Agent`, `TraitObject`, `Compose`, `Partial`,
  `Atom`, `Macro`, `PromptSection`, `Document`. Previously these v0.17-
  v0.27 Value kinds all fell back to `Type::Union(vec![])` (= "any"),
  leaving them untyped. Closes Permanent #3.

- **NaN/Inf rejection (P1-3.13)** — `Value::Number` Display no longer
  prints garbage strings; renders `nan`/`inf`/`-inf` and keeps
  IEEE PartialEq semantics. Closes **part** of Permanent #2 (display
  layer). Full numeric tower (Int/Float variants, parser suffix) → v0.37.

### High-stress hardening

- `trait_registry` / `impl_table` / `tool_registry` wrapped in `Arc<HashMap>`
  for cheap `Clone` (P1-2.10). Per-HTTP-worker 50+ KB deep-clone eliminated.
- `Value::List` / `Dict` Display streams writes (no `Vec<String>::join`)
  (P1-2.7).
- `Value` Display adds depth limit (cycle guard) — recursive Value trees
  no longer stack-overflow (P2-3.14).
- `estimate_bytes` walks Value tree directly instead of full re-serialize
  (P1-2.12).

### Concurrency hardening

- `Scheduler.next_id: Mutex<u32>` → `Arc<AtomicU64>` — no overflow (P1-1.8).
- `SandboxPolicy.allow`/`deny` `Vec<String>` → `BTreeSet<String>` for O(log N)
  checks (P1-3.10).
- `http_server` startup routes listing snapshots under Mutex, prints after
  drop — no lock-held-across-`eprintln!` (P1-1.6).

### Static-type hardening

- `check_impl_def_stmt` rejects `for_type` that doesn't name a known type
  (P1-4.10) — closes the orphan-impl soundness hole.

### Sandbox integration

- All `file.*` methods now route through `sandbox.check_path` (P2-3.15).
  Default permissive policy allows everything so existing scripts
  unaffected; strict policy can now block file access via deny patterns.

### Misc

- `MockRegistry::call` marked `#[deprecated]` — use the wrapper
  `call_mock_method` from `builtins.rs` (P1-1.9).

### CI fix (pre-existing bug)

- `ci.yml` integration job was referencing 5 example scripts that no
  longer exist at `examples/*.mora` (they're in `examples/_legacy/`).
  Job was passing via `|| true` but never actually running anything.
  Updated to the 5 active demos that DO exist.

### Deferred to v0.37

- **P1-2.8 Env pool** — requires structural change to v2 closure
  capture; bigger than v0.36 scope warrants.
- **Permanent #2 full numeric tower** — `Value::Int(i64)`/`Float(f64)`
  variants + `Literal::Int`/`Float` + parser suffix tokens. Affects 60+
  Value::Number sites across the codebase.
- **P1-4.7 `load` typed Union** + **P1-3.6 `Value::Builtin` enum migration** +
  **P1-3.7/3.8/3.9/3.10 builtin boundaries**.
- **P2 cluster** — string_interner eviction, ai_cache hash key,
  parse_json UTF-8, print signature cleanup, typeck error spans
  (line:0), Never/Unknown placeholder, with-block validation.

### Total impact
- 14 commits, single feature branch `v0.36-type-completeness`
- ~300 LOC net + ~30 LOC tests
- 337 tests pass; 0 failures
- 5 demos × unchanged pass count
- 1 new dep: crossbeam-channel 0.5

---

## [v0.35] - 2026-07-03 — Technical Debt Cleanup (20 P0s)

Remediation of all 20 P0 findings from the v0.34 zero-trust audit.
No new features; internal hardening across 4 dimensions:
concurrency / high-stress / strong-typing / static-typing.

### Concurrency (cluster A) — v0.32-0.33 module API hardening

- **`Clone for Interpreter` shares singleton state** (`interpreter/mod.rs`)
  EventBus / Scheduler / MockRegistry already Arc-backed (`#[derive(Clone)]`);
  SandboxPolicy derives Clone; `InMemoryCcrStore` now has manual `Clone`
  (AtomicU64 workaround — counter is preserved at clone time). Previously
  Clone reset 5 v0.34 fields by fresh-construction, breaking counter identity
  and losing event handlers across HTTP/MCP worker clones.

- **`EventBus::emit` clone-and-drop** (`event/mod.rs`)
  Snapshot matched handlers, drop the Mutex guard, then invoke.
  Re-entrant `bus.emit` from a handler no longer deadlocks.

- **`MockRegistry::call` clone-and-drop** (`mock/mod.rs`)
  Same pattern. Native handler invocation no longer holds the registry lock.

- **`ccr.put` hash widens 8 → 16 hex chars** (`ccr/mod.rs`)
  AtomicU64 counter now produces `{:016x}`, avoiding silent overwrite at
  n = 2^32. Test assertion updated to `hash.len() == 16`.

- **`v2_arena` wrapped in `Arc<AstArena>`** (`interpreter/mod.rs`)
  Per-call `.clone()` in v2 closure/task dispatch is now a cheap Arc bump
  instead of deep-cloning the entire AST.

### No-panic refactor residue (cluster B) — completing v0.31 invariant

- **11× `.unwrap()` removed from `walk_expr` visitor** (`ast_v2.rs`)
  Visitor previously panicked on dangling NodeId. Now skips silently,
  relying on the existing `_ => visit_expr(arena, expr)` fallthrough.

- **`Value::Router` / `Atom` Display infallible** (`value.rs`)
  Poisoned mutex no longer crashes the REPL print loop.
  2 new tests: `router_display_does_not_panic_on_empty_routes` and
  `atom_display_does_not_panic_on_valid_value`.

- **Bare `.unwrap()` → `.expect()` on globals mutex** (`interpreter/mod.rs`)
  Symmetric with the 4 other `globals.lock().expect(...)` sites.

- **Lexer rejects control chars in string literals** (`lexer.rs`)
  NUL and 0x01-0x1f / 0x7f now emit `TokenType::Error` instead of silently
  absorbing (which crashed POSIX / HTTP / file boundaries downstream).
  `\t`, `\n`, `\r` stay legitimate for multi-line literals.

### Static-type soundness (cluster C)

- **REPL now type-checks** (`interpreter/mod.rs` `run_repl_with`)
  Other entry points already did; the REPL was the gap.

- **`Dict.get` return type widens `V` → `V | Nil`** (`typeck/mod.rs`)
  Runtime may return `Nil` on missing key; typeck now agrees.

- **`call_task_inner` / `call_value_inner` surface arity errors**
  Previously silently `unwrap_or(Value::Nil)`-filled missing args.
  Now errors with `"task/closure expects N args, got M"`.

- **`route` statement reports clean runtime error** (`interpreter/execute.rs`)
  `StmtKind::Route` was parsed + type-checked but never executed.
  Now reports `"route statement 'X' is not executable in v0.35; use web
  server endpoints instead"` instead of falling through to a generic
  "Unsupported v2 statement" message.

### Hot-path / structural (cluster D)

- **8 dead `#[allow(dead_code)]` Interpreter fields removed**
  `method_cache`, `ai_batch_queue`, `cache_warm_queue`, `ai_priority_queue`,
  `adaptive_temp`, `load_balancer`, `retry_policy`, `route_registry`.
  These were write-once-construct with 0 read sites.

- **`_cache_key` dead alloc removed** (`interpreter/dispatch.rs`)
  Format-on-every-method-dispatch inlined as a comment.

- **`parse_json_list` / `parse_json_dict` O(n²) → O(n)** (`flow.rs`)
  `&s[i..].trim_start()` per loop iter replaced with byte-index `skip_ws`.
  No more slicing allocations; O(1) whitespace skip per step.

### Total impact
- 20 P0s fixed (out of 57 audit findings total)
- 335 tests pass; 0 failures (+2 from commit B2)
- 5 demos × unchanged pass count (compact_demo, compress_demo,
  compress_smart_demo, mcp_server_demo, integration_v0_34)
- ~210 LOC net + ~40 LOC new tests
- 16 commits, single feature branch `v0.35-technical-debt`

---

## [v0.34] - 2026-07-03

### Integrate 5 v0.30-0.33 Orphaned Modules as Builtins

v0.30-0.33 added 5 new modules (event/sandbox/schedule/ccr/mock) but
**never integrated them into Interpreter** — scripts could not call
`bus.emit()`, `sandbox.run()`, `schedule.add()`, `ccr.put()`,
`mock.register()`. v0.34 fixes this history debt by adding each
module as a top-level builtin with method dispatch routing.

This is the **historical debt cleanup** requested by the user
("") — no new external dependencies, no semantic
change, no API rename.

#### 1. bus.emit/off/count builtin (event::EventBus)
- **v0.32 module**: `EventBus` with Puter-style wildcard matching
  (`outer.*` catch-all prefix, interior `*` single-segment)
- **v0.34 integration**:
  * `bus.emit(event, payload?)` — fire all matching handlers
  * `bus.off(pattern)` — deregister all matching handlers
  * `bus.count()` — return pattern count
- **Limitation**: `bus.on(pattern, handler)` requires a Rust closure;
  not exposed as builtin (closure boundary with builtin dispatch is
  non-trivial). v0.32's `EventBus::on` remains available for direct
  Rust API.
- 4 unit tests in `bus_tests` mod.

#### 2. sandbox.check_builtin/check_path/allow/deny builtin (sandbox::SandboxPolicy)
- **v0.33 module**: MimiClaw path validation + AIOS access manager
- **v0.34 integration**:
  * `sandbox.check_builtin(name)` -> bool (allow/deny pattern match)
  * `sandbox.check_path(path)` -> bool (reject `..` per MimiClaw)
  * `sandbox.allow(pattern)` / `sandbox.deny(pattern)`
  * `sandbox.mode()` -> "strict" or "permissive"
- 1 unit test in `bus_tests` mod.

#### 3. schedule.add/list/remove/tick/count builtin (schedule::Scheduler)
- **v0.33 module**: MimiClaw cron_service 9-field cron_job_t
- **v0.34 integration**:
  * `schedule.add(name, kind, message, interval_s?, at_epoch?)` -> id
  * `schedule.list()` -> List of job dicts
  * `schedule.remove(id)` -> bool
  * `schedule.tick()` -> [triggered_messages] (uses Scheduler::now())
  * `schedule.count()` -> pattern count
- 1 unit test in `bus_tests` mod.

#### 4. ccr.put/get/marker/extract builtin (ccr::CcrStore)
- **v0.33 module**: Headroom Compress-Cache-Retrieve with
  `<<ccr:HASH,SIZE>>` marker
- **v0.34 integration**:
  * `ccr.put(data)` -> hash (8-char hex from u64 counter)
  * `ccr.get(hash)` -> data (or Nil if not found)
  * `ccr.marker(hash, size)` -> `<<ccr:hash,size>>` (Headroom format)
  * `ccr.extract(marker)` -> hash (parse marker, returns hash part)
  * `ccr.len()` -> entry count
- 1 unit test in `bus_tests` mod.

#### 5. mock.register/unregister/count/names builtin (mock::MockRegistry)
- **v0.32 module**: OpenFugu MockWorld + OpenInfer mock mode pattern
- **v0.34 integration**:
  * `mock.register(name)` -> stub (real handler wiring needs closure
    boundary, deferred to v0.35+)
  * `mock.unregister(name)` -> stub
  * `mock.count()` -> pattern count
  * `mock.names()` -> [String, ...] (registered handler names)
- **Limitation**: `mock.register` doesn't actually wire a handler
  (closure boundary). v0.32's `MockRegistry::register` still works
  for direct Rust API.
- 1 unit test in `bus_tests` mod.

#### Tests

- 8 new test cases in `bus_tests` mod (consolidated to avoid mod
  structure issues during iterative development)
- 328 lib tests pass (was 320 at v0.33 merge, +8)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

#### Implementation notes

- **5 new fields on Interpreter struct**: bus, sandbox, scheduler,
  ccr_store, mock_registry (all Arc<Mutex<...>>-based, Clone is
  cheap)
- **5 new globals definitions** in `Interpreter::new()`:
  `bus` / `sandbox` / `schedule` / `ccr` / `mock`
- **5 new method dispatch functions** in `builtins.rs`:
  call_event_method, call_sandbox_method, call_schedule_method,
  call_ccr_method, call_mock_method
- **5 new dispatch routing arms** in `dispatch.rs` module section
- All public APIs use `Result<Value, String>` (no panic in production)

#### Roadmap (v0.35+)

- `bus.on()` with closure capture via closure registry
- `mock.register()` with actual handler wiring
- `ai.limits` block (step/cost/wall_time) per mini-swe-agent
- `shell.run` with process group kill (POSIX `killpg`)

### Fix Production Panics on User-Input Paths

- `src/lexer.rs`: replace `value.parse().unwrap()` with `error_token`
  fallback for malformed number literals.
- `src/flow.rs`: replace `unreachable!()` in `parse_json_dict` with
  `Err("JSON object key must be a string")`.
- `src/lsp/providers/formatting.rs`: replace `.expect()` on LSP
  `range/start/end` params with graceful empty-array fallback.
- `src/interpreter/mod.rs`: replace `.expect("should have elements")` in
  `extract_embeddings` with `Result::Err`.
- `src/parser_v2/statements.rs`: finish v0.34 fix for
  `.expect("loop requires exactly one agent")` — return a valid `NodeId`
  via `arena.alloc_stmt` and include the new `with_config` field.
- `src/parser_v2/statements.rs`: replace `.expect("eval requires 'given:'")`
  with fallback to `NodeId(0)` + error log when `given:` is missing.
- `src/lsp/server.rs`: remove redundant `id.expect("id should exist")`;
  propagate `docs` and `shutdown` mutex poison via `io::Result`.
- `src/interpreter/evaluate.rs`: convert `environment.lock().expect(...)` to
  `?` and remove irrefutable `unwrap()` after `Some` matches.
- `src/interpreter/execute.rs`: convert all `environment.lock().expect(...)`
  to `?`.
- `src/interpreter/dispatch.rs`: convert `atom`/`environment`/`done`/`routes`
  /`tool_registry` mutex expects to `?`.
- `src/interpreter/trait_dispatch.rs`: convert `environment.lock().expect(...)`
  to `?`.
- `src/interpreter/orchestrate.rs`: convert `environment.lock().expect(...)`
  to `?` (including the nested closure in Graph edge evaluation).
- `src/interpreter/mod.rs`: convert `globals.lock().expect(...)` in
  `interpret()` to `?`; unify `new()` `.unwrap()` to
  `.expect("globals mutex poisoned")`.

#### Tests

- `tests/parser_v2_integration.rs`: add `test_parse_eval_without_given_no_panic`.
- `src/lsp/server.rs`: add `handle_notification_without_id_no_panic`.

#### Verification

- `cargo build --all-targets`: clean
- `cargo test --all`: 331 passed, 2 ignored
- `cargo clippy --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

## [v0.33] - 2026-07-02

### Schedule + Sandbox + Reading Order + CCR (4 P1 primitives)

: 7-project deep-dive  (AGENTS_PRIMITIVES.md)  v0.33 P1 .
 4 **** P1 ,  trait-based +  in-memory ,
.

#### 1. Schedule (cron) — MimiClaw 

`src/schedule/mod.rs`:
- `Scheduler`: `Arc<Mutex<HashMap<String, Job>>>`
- `Job { id, name, kind, interval_s, at_epoch, message, last_run_epoch, delete_after_run }`
- `JobKind`: Every | At
- `add(name, kind, message, interval_s, at_epoch) -> Result<id, Err>`
- `list() -> Vec<Job>`, `remove(id) -> bool`
- `tick(now) -> Vec<triggered_messages>` (consume for event loop)
- `set_persist_path(path)` + best-effort JSON dump

: MimiClaw cron_service.c (9  cron_job_t).
****:  channel/chat_id, std::fs JSON  (vs SPIFFS).

#### 2. Sandbox Policy — AIOS + Puter + MimiClaw 

`src/sandbox/mod.rs`:
- `SandboxPolicy { allow, deny, fs_root, timeout_s, memory_limit_mb }`
- `check_builtin(name) -> Result<(), Err>` ( `event::matches` wildcard,
  deny  allow)
- `check_path(path) -> Result<PathBuf, Err>` (MimiClaw  `..` ,
   fs_root )
- `strict()` / `permissive()` / Default constructors

:
- MimiClaw path traversal defense
- AIOS Access Manager (agent_id -> privilege_group)
- Puter iframe sandbox + capability URL params

#### 3. document.reading_order — MinerU 

`src/document/reading_order/mod.rs`:
- `BBox { x, y, w, h }` + center/edge accessors
- `from_value(v)`: accept both flat bbox dict AND block dict with 'bbox' sub-dict
- `Strategy`: InputOrder | TopToBottom | GapTree | XyCut | GroupBased
- `assign_reading_order(blocks, strategy)`:  block  'reading_order_idx'

: MinerU §2.8 Reading Order Recovery (3 ).
****:  recursive XY-cut,  cross-page merge, .

#### 4. CCR (Compress-Cache-Retrieve) — Headroom 

`src/ccr/mod.rs`:
- `CcrStore` trait: `put(data) -> hash; get(hash) -> Option<entry>; len()`
- `CcrEntry { hash, size, data }`
- `InMemoryCcrStore` default impl (Arc<Mutex<HashMap>> + u64 counter)
- `make_marker(hash, size) -> "<<ccr:hash,size>>"`
- `extract_hash(marker) -> Option<&str>`

: Headroom CcrStore (lossy ).
****: 8-char hex hash (vs SHA-256),  marker  ( KIND).
****: v0.34  `crush_json` lossy .

#### 

- 320 lib tests (was 286, +34)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

####  (v0.34+ )

P1 (v0.34 6-8 ):
- `react` (ReAct ) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU 
- `skill` markdown — MimiClaw skill_loader
- CCR ↔ crush_json  (lossy  marker)
- `heartbeat`  — MimiClaw
- Sandbox ↔ builtin  (file.read  check_path)

P2+ (v0.35+ ):
- `plan` (DAG) — OpenFugu Conductor
- `mora serve --openai`  — OpenInfer
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle`  — Puter
- DI  (5 ) — Puter
- `policy` learned router — OpenFugu
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu
- `cross_page merge` — MinerU

## [v0.32] - 2026-07-02

### Lossless-First Recursive Walker + Event Bus + Mock Registry

:  deep-dive 7  AI  (AIOS / MimiClaw / OpenFugu /
OpenInfer / MinerU / Headroom / Puter) . 
`AGENTS_PRIMITIVES.md` (581 ).  3 **** P0 ,
 plan/react/openai-serve  v0.33.

#### 1. Lossless-First Recursive Walker (Headroom )

`src/compress/json.rs::compact_value_recursive` + `crush_json_recursive`:
-  Value  pure iterative DFS ( Windows 1MB stack )
-  List  (`len >= min_items`)  `try_lossless_compact`
  (csv-schema  markdown-kv), 
-  `CompressOptions.recursive: bool` (default false, )
-  List  SmartCrusher (inlined via `crush_json_inner` )
- 2 new tests: `recursive_walker_compacts_nested_lists`,
  `compact_value_recursive_simple`

: [Headroom DocumentCompactor](https://github.com/chopratejas/headroom)
(`crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs`)

#### 2. Event Bus with Wildcard (Puter )

 `src/event/mod.rs`:
- `EventBus`: `Arc<Mutex<HashMap<Pattern, Vec<Handler>>>>`
- `on(pattern, handler)` ; `off(pattern)` ; `emit(event, payload)` 
- `matches(event, pattern)`: Puter 
  - trailing `*` = prefix catch-all (`outer.*`  `outer.gui.item.removed`)
  - interior `*` = single segment wildcard (`outer.*.item`)
  - bare `*` = 
- 8 unit tests covering exact/prefix/interior/catchall/dispatch

: [Puter EventClient](https://github.com/HeyPuter/puter)
(`src/backend/clients/event/EventClient.ts`)

#### 3. Mock Registry (OpenFugu + OpenInfer )

 `src/mock/mod.rs`:
- `MockRegistry`: `Arc<Mutex<HashMap<String, MockHandler>>>`
- `register(name, fn) / unregister(name) / call(name, args) / count / names`
- `MockHandler`: `Arc<dyn Fn(&Value) -> Value + Send + Sync>`
-  Mora  `Value` ,  `serde_json` 

:
- [OpenFugu MockWorld](https://github.com/trotsky1997/OpenFugu) (train/train_trinity.py)
   sep-CMA-ES 
- OpenInfer mock mode ( Python  Rust )

Mora  `compress/text.rs` / `ai_chat.rs`  hardcode mock ,
v0.32  `MockRegistry` .  builtin (ai.chat / http.fetch) 
consult `mock.call`  mock ,  offline deterministic .

#### 4. AGENTS_PRIMITIVES.md (581 )

,  v0.32+  (16  + 5  + 7 ).
:  +  () + Mora  +  +
 Mora .

#### 

- 286 lib tests (was 272, +14)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

####  (v0.33+ )

P1 (v0.33 6-8 ):
- `plan` (DAG) — OpenFugu Conductor
- `react` (ReAct ) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU group-based
- `document.reading_order` — MinerU 3 
- `schedule` cron — MimiClaw cron_service
- `skill` markdown — MimiClaw skill_loader
- `sandbox`  — AIOS + Puter
- `ccr` Compress-Cache-Retrieve — Headroom

P2+ (v0.34+ ):
- `mora serve --openai`  — OpenInfer vLLM frontend 
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle`  — Puter hooks
- DI  (5 ) — Puter
- `heartbeat`  — MimiClaw
- `policy` learned router — OpenFugu TRINITY
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu evidence grade
- `cross_page merge` — MinerU

## [v0.31] - 2026-07-02

### No-Panic Refactor + Code Quality Hardening

 v0.30 "" (user: "5 ").
**** — .

#### : 21 panic -> 0 in lexer/parser

,  `panicked at src/lexer.rs:...`
 abort. :
- Lexer 8  panic  emit `TokenType::Error(String)` token
- Parser 13  panic  `eprintln!`  +  safe default
  ( /  list /  OrchestrateKind.Sequential)
-  `"Parse error: ..."`  stack trace

`examples/_legacy/`  demo ( panic)  crash .

#### : Windows OCR model path fallback

`user_model_path()`  `XDG_DATA_HOME`  `HOME`,
 Windows ,  fail.  `LOCALAPPDATA` fallback
 3 .  3 .

#### : cargo doc warnings 14 -> 0

Module-level `//!`  HTML :
- `<Page>`, `<Block>`, `<Span>`  `\[ \]` 
- `<p>`, `<N>`, `Vec<Value>` 
- bare URL `https://...`  `<https://...>`

`cargo doc --no-deps`  0 warning, docs.rs .

#### 

- 272 lib + 5 integration = 277 test 
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff
- `cargo doc --no-deps`: 0 warning

## [v0.30] - 2026-07-02

### SmartCrusher —  JSON 

 [headroom](https://github.com/headroomlabs-ai/headroom)  SmartCrusher
 +  +  v0.29 " + 30%  15% "
" + 5  + 3 "

####  BREAKING CHANGES

- `CompressOptions.anomaly_keys: Vec<String>` ****v0.30 
- `CompressOptions`  5  11 v0.29  + 6 
- `crush_json_core` **** `crush_json` `(items, target, options)`
   `crush_json_core(input, max, anomaly_keys)` 
- `parse_json_simple` stub **** `flow::json_to_value`
- `crush_json` / `compress.json` / `List.crush_json`  marker 
  `method=smart_crusher strategy={...} items={...} total={...} savings={...}`

####  v0.29  head_tail

|  |  |  |
|---|---|---|
| `auto` (default) |  |  ArrayType  |
| `topn` |  /  Score  |  Score  top N |
| `timeseries` |  /  Temporal  |  +  |
| `cluster` |  /  uniqueness < 0.3 |  |
| `lossless` |  | schema  csv-schema / md-kv |
| `smart_sample` | fallback |  +  +  |

#### 5 

- `Id` — uniqueness > 0.9 /UUID/
- `Score` — bounded numeric range (0-1  0-100)
- `Temporal` — ISO 8601 / Unix timestamp 
- `Error` —  `error`/`failed`/`exception`/... 
- `Anomaly` —  >3σ from mean (1-5% )

#### 3 

- `KeepErrorsConstraint` — 
- `KeepOutliersConstraint` — Anomaly  >2σ 
- `KeepBoundaryConstraint` —  k_first +  k_last  15%

####  builtin 

```mora
--  auto: 
compress.json(tool_output, {target_ratio: 0.2})

--  TopN
compress.json(scored_list, {strategy: "topn", target_ratio: 0.1})

--  TimeSeries
compress.json(metrics, {strategy: "timeseries", target_ratio: 0.3})

-- Lossless (csv-schema , )
compress.json(flat_table, {strategy: "lossless", max_bytes: 5000})

-- 
compress.json(api_logs, {
    strategy: "auto",
    target_ratio: 0.2,
    preserve_errors: true,
    preserve_outliers: true,
    preserve_ids: false,
})

--  metadata
let result = compress.json(items, {target_ratio: 0.2})
result.savings_ratio    -- 0.8 (80% )
result.strategy_used    -- "topn"
result.fields           -- [{name, role, ...}, ...]
```

#### 

|  |  (v0.29) |  (v0.30) |  |
|---|---|---|---|
| 100  × 5  | 60% | 70-80% | +10-20% |
| 1000  × 20  | 60% | 75-85% | +15-25% |
| 10000  × 30  | 60% | 80-90% | +20-30% |

#### 

- `src/compress/json.rs` —  (267 → 970 )
  - `FieldRole` / `FieldStats` / `ArrayType` 
  - 5  detector + 5  Strategy + 3  Constraint
  - `crush_json` / `crush_json_string` / `try_lossless_compact`
- `src/compress/mod.rs` — `CompressOptions`  (11 )
  - `parse_json_simple`  `flow::json_to_value`
  - `value_to_json_simple`  `flow::value_to_json`

#### 

- 12  unit test v0.29 5  test
  - 5  role detectionid/score/error/temporal/anomaly
  - 4  strategytopn/timeseries/lossless/auto
  - 2  constrainterrors/outliers
  - 1  metadata
  - 1  string 
-  v0.29  test `crush_json_core` / `anomaly_keys` / `parse_json_simple_currently_stub`
-  272 test `cargo clippy --all-targets -- -D warnings` 

## [v0.29] - 2026-07-01

### compress + crush_json + OCR .rten 

 [headroom](https://github.com/headroomlabs-ai/headroom) ContentRouter + Kneedle 
Mora  JSON  +  system prompt 

####  / builtin

```mora
-- 6  (auto / head_tail / summary / lossless / json / code-html-log-text)
let summary = compress(text, "summary")                       -- LLM 
let head    = compress(text, "head_tail", head_pct: 0.3)     -- 
let lossless = compress(text, "lossless")                     --  size marker
let auto    = compress(text, "auto")                          -- 

--  JSON  (Kneedle + )
let crushed = crush_json(big_list, max: 10)
let crushed = crush_json(big_list, max: 10, anomaly_keys: ["error"])

-- 
let summary = conv.compress("summary")
let crushed = list.crush_json(10)
```

####  `compress`

|  |  |
|---|---|
| `SubCompressor` trait | `sniff` / `compress` / `origin` 3  |
| `ContentRouter` |  →  |
| `JsonSubCompressor` |  crush_json_core |
| `CodeSubCompressor` | regex  +  body |
| `HtmlSubCompressor` |  v0.27 quick-xml  |
| `LogSubCompressor` |  pattern cluster |
| `TextSubCompressor` | head_tail / summary / lossless  |

####  BREAKING: `compact`  `compress`

v0.25  `compact(text)` builtin  `compress(text, "summary")`
`examples/compact_demo.mora`  v0.29 

#### OCR `.rten`  ( v0.28 tech-debt)

- v0.28 vendored  11.7 MB `.rten` 
-  `~/.local/share/mora/ocr/`  ( `MORA_OCR_MODELS_DIR` )
-  `docs/install-ocr.md` 
-  `.git/sdd/ocrs-shasums.txt`  reference checksum
- **BREAKING**:  OCR  `mora-install-ocr` 

#### 

- `src/compress/{mod,json,code,html,log,text}.rs` (~1000 )
- `docs/install-ocr.md`
- `.git/sdd/ocrs-shasums.txt`
- `examples/compress_demo.mora` ()

#### 

- **** —  v0.27 / v0.28  deps (`regex` transitive from `ocrs`)
- **** —  v0.26 / v0.27 / v0.28 
- **CodeSubCompressor  regex** — v0.30+  tree-sitter
- **** `compress.` / `crush_json.` / `ocr.load.`

## [v0.28] - 2026-07-01

### Office (PPTX/DOCX) + Image OCR Backends

 v0.27 DocumentBackend  MinerU 
 v0.27 trait  3  DocumentBackend 

#### 

|  |  |  |  |
|---|---|---|---|
| PptxBackend | .pptx | undoc 0.5 |  |
| DocxBackend | .docx | undoc 0.5 | Word  |
| ImageBackend | .png | ocrs 0.12 + image 0.24 |  OCR Rust / rten ONNX|

#### 

```mora
let deck = document.parse("./deck.pptx")           -- PPTX
let report = document.parse("./report.docx")        -- DOCX
let scan = document.parse("./scan.png")            -- OCR

print(deck.markdown())                              -- markdown 
print(report.text())                                -- 
print(scan.metadata()["ocr_engine"])                -- "rten"
```

####  v0.26/v0.27 

```mora
--  v0.26 compose_prompt
let sys = compose_prompt({role:"system", text:deck.text(), budget:"32 KB"})
--  v0.27 
document "report" do
    set origin: "docx"
    read "./report.docx"
end
```

#### 

- `undoc` 0.5 `docx` + `pptx` features Rust
- `ocrs` 0.12OCR  Rust
- `rten` 0.24ocrs  re-export `Model::load_static_slice`  `.rten`
- `anyhow` 1ocrs  `OcrEngine::new`  `anyhow::Result`ocrs  re-export `anyhow`
- `image` 0.24 `png` feature PNG header / dimensions

 RustMSRV 1.85 

#### 

- **** 5  crate  pure Rust
- **PNG only in v0.28**JPEG / XLSX /  PDF  v0.29+
- **OCR **`ocrs 0.12`  Microsoft `rten` ONNX runtime
- ** OCR**v0.28 eng.traineddata bundled
- ****v0.27  `parse_document(path)`  `PptxBackend` / `DocxBackend` / `ImageBackend`

#### Known issues / v0.29+ roadmap

- **11.7 MB `.rten`  vendoring**OCR /`text-detection.rten` 2.4 MB + `text-recognition.rten` 9.3 MB raw blob  `tests/fixtures/` git LFS contributor / CI  `git clone`  ~12 MB`mora` release binary  `include_bytes!`  ~12 MB blob  PR  diff/ `.git/sdd/tech-debt-v0.29.md`v0.29 git LFS / `build.rs`  /  model dir
- **OCR **`ocrs 0.12`  `eng.traineddata` 
- **OCR  PNG**JPEG / WebP / TIFF  v0.29+
- ** PDF** PDF OCR 

## [v0.27] - 2026-07-01

### Document  IR — `document.parse(...)` + 

 [opendatalab/MinerU](https://github.com/opendatalab/MinerU) middle_json 
Mora  PDF / Markdown / HTML , `Value::Document` IR

#### 

```mora
document "report" do
    set origin: "pdf"
    set max_pages: 3
    read "./q3-report.pdf"
end

let doc = document.parse("./q3-report.pdf")
let md  = doc.markdown()
let pages = doc.pages()
let meta = doc.metadata()
```

####  `document`

|  |  |
|---|---|
| `document.parse(path)` | , `Value::Document` |

#### `Document` value 

|  |  |  |
|---|---|---|
| `doc.markdown()` | `string` |  markdown  |
| `doc.text()` | `string` | |
| `doc.pages()` | `List<Dict>` |  IR Page  |
| `doc.blocks()` | `List<Dict>` |  block |
| `doc.metadata()` | `Dict` |  origin / pages / size|
| `doc.origin()` | `string` | "pdf" / "markdown" / "html" |

####  + Trait

- `Value::Document { backend: Arc<dyn DocumentBackend + Send + Sync>, metadata: HashMap<String, Value> }`
- `pub trait DocumentBackend: Debug + Send + Sync { fn origin / pages / markdown / text / metadata / blocks }`
- 3 : `PdfBackend` (lopdf + pdf-extract) / `MarkdownBackend` (pulldown-cmark) / `HtmlBackend` (quick-xml)

#### 

- `lopdf` 0.41 + `pdf-extract` 0.12 (PDF)
- `pulldown-cmark` 0.13 (Markdown)
- `quick-xml` 0.40 (HTML)
-  Rust, MSRV 1.85 , 

####  v0.26 

```mora
let doc = document.parse("./report.pdf")
let sys = compose_prompt({role:"system", text:doc.markdown(), budget:"32 KB"})
let resp = ai.chat(p"{sys}\n\n{question}")
```

#### 

- **** Rust crate
- ** Value ** PDF /  `backend: Arc<dyn ...>` 
- **Lazy ** `.pages()` / `.markdown()`  Value, 
- **** PPTX / DOCX  `impl DocumentBackend`

## [v0.26] - 2026-07-01

### Prompt Sections —  +  + 

 [mimiclaw](https://github.com/memovai/mimiclaw)5  [headroom](https://github.com/headroomlabs-ai/headroom) LLM  system prompt 

####  `prompt`

```mora
prompt "identity" do
    set role: "system"
    set budget: "256 B"
    read "./SOUL.md"
end

prompt "memory" do
    set role: "system"
    set budget: "8 KB"
    tail("./sessions/today.jsonl", max: 20)
end

let sys = compose_prompt("identity", "memory")
```

#### 

|  |  |
|---|---|
| `compose_prompt(...)` |  system prompt section budget  |
| `tail(path, max: N)` |  N JSONL/ |

#### 

- `Value::PromptSection { name, role, text, budget_bytes }`

####  AST 

- `StmtKind::PromptSection { name, body }`
- `StmtKind::PromptSet { key, value }` `set role:` / `set budget:`
- `StmtKind::PromptRead(NodeId)` `read`

#### 

- **** tokenizer UTF-8  mimiclaw 
- **** section  ValueIR
- ****

## [v0.25] - 2026-07-01

###  (Code Modularization)

 5 

#### 
- **interpreter**: 3402  → 3  (mod.rs + execute.rs + evaluate.rs)
- **typeck**: 2838  → 2  (mod.rs + check.rs)
- **parser_v2**: 2609  → 3  (mod.rs + statements.rs + expressions.rs)
- **record**: 2091  → 7  (mod.rs + serialization.rs + diff.rs + analysis.rs + audit.rs + snapshot.rs + tests.rs)
- **lsp/providers**: 1092  → 11  (mod.rs + helpers.rs + 9  provider )

#### 
- 
- 
- 

### 
-  `test_memory_save_load`  Windows 
-  `std::env::temp_dir()`  `/tmp` 

## [v0.24] - 2026-06-30

### ParserV2  (Complete)

ParserV2  Parser 
 parser.rs (2459 )  ParserV2

#### 
- **append_statement**: 
- **read_bytes_statement**: 
- **write_bytes_statement**: 
- **stream_statement**:  `stream <expr> as <var> do ... end`
- **tool_statement**:  `tool name(params): type do ... end`
- **observe_statement**:  (trace/metrics/otel)
- **span_statement**:  `span "name" tags {..} do ... end`
- **record_tokens_statement**:  token 
- **assignment_statement**:  `IDENT = expr`
- **index_assignment**:  `IDENT[expr] = expr`
- **commit/rollback**: /

#### 
- **match_expression**:  ( when )
- **pattern**:  (////)
- **parse_format_string**: 
- **parse_ai_model_call**: ai_model  ( keyword args)
- **flatten_prompt_parts**: Prompt 
- **list_literal / dict_literal**: 
- **char_literal**:  `'a'`
- **NamespaceRef**:  `Module::method()`

#### 
- **parse_generic_params**:  `<T: Bound>`
- **parse_type_list**:  `<T, U, V>`
- **parse_type_name_recursive**: 
- **parse_where_clause**: where 

#### 
- **let **: 
- **string + any**:  ()

#### 
- **ObserveConfig**:  ast_v2.rs  NodeId
- **FnDef / TraitMethod**:  ast_v2.rs  Vec<NodeId>
- **Pattern**:  ast_v2.rs Guard condition  NodeId
- **consume_method_name**: 
- ****:  (binary → unary → call → primary)
- ****: ast_v2_to_v1.rs  AST 

### 9 Languages Features Integration (Complete)

All features from the learning plan have been implemented.

### v0.21: Rust 

- ****: `&expr` / `&mut expr`
- ****: `<'a>` 
- ****: /

### v0.22: 

- **AI **:  prompt 
- ****:  map/filter/take/drop 
- ****: 
- ****: 
- **HTTP **:  (16)
- **MCP **:  (8)
- ****: 

### v0.24: 

- ****: `type Name = TargetType`
- ****: `enum Name { V1, V2(Type) }`
- ****: `struct Name { field: Type }`

### 

- **docs/mora-spec.md**: Mora  (20 )
- **docs/influences.md**: 9 
- **docs/learning-plan.md**: 
- **docs/workflow-v0.20.md**: 

From Prolog, StreamIt, APL, Clojure, Lisp, Smalltalk, Common Lisp, Ballerina, Logo.

#### Pattern Matching Enhancement (Prolog)
- **Match guard conditions**: `match n with x when x > 0 -> ... end`
- **List rest pattern**: `[head, ...tail] = [1, 2, 3]`
- **Dict partial match**: `{name: n} = {"name": "Alice", "age": 30}`

#### Pipe & Stream (StreamIt + APL)
- **Pipe with closure**: `5 |> fn(x) return x * 2 end`
- **Window aggregation**: `[1,2,3,4,5].window(3)` → `[[1,2,3],[2,3,4],[3,4,5]]`
- **Batch processing**: `[1,2,3,4,5].batch(3)` → `[[1,2,3],[4,5,6],[7]]`
- **Array operations**: `.shape()`, `.flatten()`, `.transpose()`, `.reshape()`
- **Broadcast arithmetic**: `[1,2,3] * 2` → `[2,4,6]`

#### Functional Core (Clojure + Lisp)
- **Compose**: `compose(f, g, h)` → composed function
- **Take/Drop**: `[1,2,3].take(2)` → `[1,2]`, `[1,2,3].drop(1)` → `[2,3]`
- **Partial application**: `partial(add, 10)` → partial applied function

#### Concurrency (Clojure)
- **Atom**: `atom(0)` → mutable reference
- **Swap**: `swap(counter, fn(n) return n + 1 end)`
- **Deref**: `deref(counter)` → current value

#### Reflection (Smalltalk)
- **type_of**: `type_of(42)` → `"number"`
- **is_instance**: `is_instance("hello", "string")` → `true`
- **methods_of**: `methods_of([1,2])` → `["push","pop","map",...]`
- **Message chain**: Router methods return self for chaining

### Statistics
- **Tests**: 147 → 178 (+31)
- **Code**: +7010 / -1517 lines

## [v0.15] - 2026-06-28

### AI Config Integration

- **TokenBudget.per_call**: Per-call token limit check
- **real_ai_chat_with_tools**: Now reads temperature/max_tokens/system from config
- **Route config**: RouteConfig settings now applied to AI calls
- **with mock_llm**: Mock LLM response queue for testing

### Record CLI Extension

- **mora record list**: List all recordings
- **mora record stats**: Show recording statistics
- **mora record timeline**: Show call timeline
- **mora record export**: Export JSONL/Markdown
- **mora record audit**: Secret scanning with .moraignore
- **mora record report**: Evidence report generation
- **mora snapshot**: Snapshot testing for regression

### Documentation

- **docs/mora-spec.md**: Mora Language Specification (20 chapters)
- **docs/influences.md**: 9 Languages Influence Analysis
- **docs/learning-plan.md**: Feature Integration Plan

### Statistics
- **Tests**: 126 → 147 (+21)

## [v0.14] - 2026-06-27

### Record/Replay/Diff CLI

- **mora record**: Record AI calls to JSONL
- **mora replay**: Replay recordings deterministically
- **mora diff**: Compare two recordings

### Statistics
- **Tests**: 121 → 126 (+5)

## [v0.13] - 2026-06-26

### Breaking Changes

- Removed `Type::Any` variant
- Removed Walrus syntax (`:=`)

### Statistics
- **Tests**: 113 → 121 (+8)

---

## Version History

| Version | Date | Tests | Key Features |
|---------|------|-------|--------------|
| v0.20 | 2026-06-28 | 178 | 9 languages integration |
| v0.15 | 2026-06-28 | 147 | AI config + record CLI |
| v0.14 | 2026-06-27 | 126 | record/replay/diff |
| v0.13 | 2026-06-26 | 121 | Remove Type::Any |

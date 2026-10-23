//! MIR 执行 — 模块入口（壳层）。
//!
//! ## 这个模块**不拥有执行实现**（v0.104.6 D467 重写）
//!
//! v0.75.47 起本文件叫「VM 执行内核」，并在注释里声称拥有
//! `execute` / `run_dag_with_signal*`。实测那是**假的**：本文件里**一行
//! 指令执行逻辑都没有**，只有一层转发 ——
//!
//! ```text
//! execute             (本文件)  → run_with_signal(本文件).1
//! run_with_signal     (本文件)  → run_dag_with_signal(engine)
//! run_dag_with_signal (engine)  → run_dag_with_signal_memo(engine)
//! run_dag_with_signal_memo     ← **唯一的指令执行循环**（engine.rs）
//! ```
//!
//! 真正操作寄存器与 env 的代码只有一份：`handlers::dispatch`
//! （`mir/inst.rs`，穷尽匹配 59 个 `MirInst` 变体），由 engine 独占调用。
//!
//! ## 重写后的结构（按「模块各司其职」组织）
//!
//! ```text
//! mir/vm/mod.rs     ← 本文件：**公共工具 + 显式入口转出**（无执行实现）
//! mir/vm/engine.rs  ← 唯一的执行引擎：建图 + BSP 超步调度
//! mir/inst.rs       ← 唯一的指令语义：dispatch（59 个变体）
//! ```
//!
//! - 本文件真正拥有的：`index_value` / `index_assign_value` /
//!   `value_to_string` / `self_match_pattern`（值层工具，与执行策略无关）、
//!   `MirSignal`、`build_task_registry`、`run_main_task`（main 入口查找）。
//! - 执行策略（何时建图、怎么调度）全部在 `engine`，本文件**不再假装**拥有它。
//!
//! ## 关于 `execute` 这个名字
//!
//! v0.104.6 D467：主入口**原名 `run_mir`**，已更名为 `execute` ——
//! 旧名暗示存在「线性执行器」，而自 v0.59 起仓库里**只有** DAG/BSP 一种
//! 执行实现。重命名让 API 名副其实。
//!
//! `execute` 实际是「建 DAG → BSP 超步调度」，与 `run_dag*` 是**同一条
//! 路径**（前者是后者的薄封装），两者共用同一个 `dispatch`。
//!
//! 若要直接用引擎，用 `engine::` 下的显式入口（`run_with_signal` 等）。
//!
//! 参数类型为 `&mut dyn MirHost`（`mir/host.rs`），解耦 mir ↔ interpreter
//! 双向依赖。`Interpreter` 实现 `MirHost`。

use std::collections::HashMap;
use std::sync::Arc;

use super::{MirFunction, MirInst};

use crate::mir::host::MirHost;
use crate::value::{Environment, Value};
/// float 模式匹配容差（`pat_str = "float:x"` 时 |实际 - x| < 此值判等）。
/// 与 eval 的 tolerance 语义一致：浮点比较用 epsilon 而非 ==。
const FLOAT_PATTERN_EPSILON: f64 = 1e-9;

/// Build a task registry from a MirFunction body.
/// Maps task name → (parameter names, body function).
pub fn build_task_registry(body: &[MirInst]) -> HashMap<&str, (&[String], &MirFunction)> {
    body.iter()
        .filter_map(|inst| {
            if let MirInst::TaskDef { name, params, body } = inst {
                Some((name.as_str(), (params.as_slice(), body.as_ref())))
            } else {
                None
            }
        })
        .collect()
}

/// MIR 解释器执行一个 MirFunction，返回最后的表达式值或 Return 值。
///
/// v0.59: 现在通过 DAG 分析执行，与 `run_dag` 共享同一套 handler 函数。
/// 构建链为 `dag_analyze → dag_optimize → prune_sequence_edges`（经
/// `MirHost::dag_cache` 缓存）。
///
/// v0.104.6 更正：本函数原注释称
/// 「v0.59: 现在通过 DAG 分析 + 强制 Sequence 边退化为线性执行 …
/// 等价于 `dag_analyze(func) → add_sequential_edges() → run_dag()`」——
/// **该表述不成立**：`add_sequential_edges` 从未出现在本函数的构建链里。
/// 且实测表明，线性化后的图交给同一个 BSP 执行器会**改变控制流语义**
/// （`if-else` / `for+break` / `while+continue` 三例发散）。
/// v0.104.6 **D466**：`add_sequential_edges` 已整体删除（`src/` 内零调用
/// 且自述不保语义）；本函数的生产链始终是
/// `dag_analyze → dag_optimize → prune_sequence_edges → run_dag`。
///
/// v0.104.6 复核更正：此段原写「`while+continue` 一例线性化反而正确、
/// 生产路径错误 —— 即缺陷 E1」。**两处都错**：生产路径的 5.0 才是对的
/// （`continue` 的 if/else 等价写法同样得 5.0，删掉 `continue` 才得 6.0，
/// 见 `tests/continue_semantics.rs`）；线性化路径的 6.0 才是错值，原因是
/// 它**忽略了 `break` / `continue`** —— 这与 `for+break` 那例的错法同源。
/// E1 早已修复而发散依旧，故原归因不成立。
/// 证据与复现见 `tests/execute_equiv_run_dag.rs`。
///
/// v0.75.9: 接收 `&Arc<MirFunction>` — 优化后 DAG 走宿主内核缓存
/// （`MirHost::dag_cache`，key = Arc 指针），同 Arc 跨调用复用。
///
/// **v0.104.6 D467 · 名字说明**：本函数**不**是「线性执行器」，也不暗示
/// 存在两套执行实现。实际路径是 [`run_with_signal`] → [`engine::run_dag_with_signal`]
/// → `engine::run_dag_with_signal_memo`（唯一执行循环）。历史上的「线性执行
/// 实现」已在 v0.59 归并进 DAG 引擎，本函数与 `run_dag*` 是**同一条路径**，
/// 共用 `handlers::dispatch`。
///
/// **v0.104.6 D467**：本函数**原名 `run_mir`**，已重命名为 [`execute`] ——
/// 旧名 `run_mir` 暗示存在「线性执行器」，而自 v0.59 起仓库里**只有**
/// DAG/BSP 一种执行实现。重命名让 API 名副其实；既有调用点已同步更新。
///
/// 需要传播 `MirSignal`（Pregel 的 `Flow::Halt` 等）时用
/// [`engine::run_with_signal`]；需要显式控制 DAG 缓存时用
/// [`engine::run_with_signal_cached`]。
pub fn execute(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<Value, String> {
    Ok(engine::run_with_signal(func, interp, env, effects)?.1)
}

/// α.1: 索引操作 List[i] / Dict[key] / String[i]
///
/// v0.104.6 修复：字符串索引**在源码层完全不可用**。
///
/// 本语言所有数值字面量都是 `Value::Float`（`str(3)` 得 `"3.0"` 可证），
/// 所以 `s[0]` 传进来的实参是 `Float(0.0)`；而下面的字符串分支只匹配
/// `Value::Int`，于是**任何** `s[i]` 都掉进末尾的 `_ =>` 兜底：
///
/// ```text
/// let s = "abc"
/// s[0]              # ERR: cannot index String("abc") with Float(0.0)
/// s[len(s) - 1]     # ERR: cannot index … with Float(2.0)
/// ```
///
/// 即「取一个字符」这个最基本的操作在源语言里根本写不出来。`Value::Int` 只能
/// 从 `len()` 一类产出 `Int` 的原语拿到，而 `len(s) - 1` 经算术后又会退回
/// `Float`（见 `flow.rs` 的 Int/Float 混合算术），所以旧签名够不着。
///
/// List 分支本来就同时收 Int 与 Float，字符串分支与之对齐即可。
/// 索引归一化：把数值下标转成 `usize`，并拒绝**负数**与 NaN。
///
/// v0.104.6 D28：负下标**静默返回首元素**。
///
/// 根因是 Rust 的浮点→整数 `as` 转换是**饱和**转换（Rust 1.45 起）：
/// `-1.0 as usize == 0`、`-5.0 as usize == 0`、`-0.0 as usize == 0`，
/// 既不 panic 也不报错。而 `Value::Float` 正是**裸数字字面量的运行期类型**
/// （`lexer.rs:702` 一律发射 `TokenType::Float`），所以 `xs[-1]` 走的就是
/// 这条分支：
///
/// ```text
/// let xs = [1, 2, 3]
/// xs[-1]     # 1.0 —— 静默给首元素
/// xs[-5]     # 1.0 —— 越界 5 位，仍给首元素
/// "abc"[-1]  # a
/// ```
///
/// 即「取倒数第 N 个」这个意图会**无声地退化成取第一个**，不报错。
///
/// 与正向越界既有的约定对齐：下标不落在 `[0, len)` 内一律报错，只是把
/// 此前被饱和转换吞掉的「负」与「非数」两种情形也纳入检查。
///
/// `what` 是错误信息里的下标措辞（`"index"` / `"string index"`）—— 字符串
/// 越界历来报 `string index N out of bounds`，别被顺手改成通用的
/// `index N`（`tests/builtin_gaps.rs` 的 D2/D3 用例锁着这条措辞，
/// 它对用户判断「是哪个空间越界了」有信息量）。
fn checked_index(n: f64, len: usize, what: &str) -> Result<usize, String> {
    if n.is_nan() {
        return Err(format!("{}: NaN is not a valid index", what));
    }
    if n < 0.0 {
        return Err(format!("negative {}: {}", what, n));
    }
    // 上界用原始 f64 比较，`as usize` 的饱和值不进消息 —— 否则
    // `xs[1e30]` 会报成 index 18446744073709551615，与源码对不上。
    if n >= len as f64 {
        return Err(format!("{} {} out of bounds (len {})", what, n, len));
    }
    // ⚠ 浮点下标**向零截断是既定设计**（v0.104.6 D3 的决定），由
    // `tests/depth_and_diagnostics.rs::d28_positive_index_unaffected` 钉住：
    //     // 浮点下标按既有约定向零截断（D3 的设计决定）
    //     assert_eq!(run("…\nxs[1.9]").unwrap(), "Float(2.0)");
    //     assert_eq!(run("…\ns[1.9]").unwrap(), "Char('b')");
    //
    // D320 曾试图改成「小数下标报错」，被该判据打回后**已回退**。
    // D320 的教训：D28（负下标）与 `method_dispatch`（越界吞成 nil）讲的是
    // **越界/负数**两条，不覆盖小数；**不能拿「下标算错必须暴露」这条原则
    // 去推「小数也该报错」** —— 原则的适用边界要看既有判据钉了什么。
    Ok(n as usize)
}

pub fn index_value(obj: &Value, idx: &Value) -> Result<Value, String> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = checked_index(*i as f64, list.len(), "index")?;
            list.get(i)
                .cloned()
                .ok_or_else(|| format!("index {} out of bounds (len {})", i, list.len()))
        }
        (Value::List(list), Value::Float(n)) => {
            let i = checked_index(*n, list.len(), "index")?;
            list.get(i)
                .cloned()
                .ok_or_else(|| format!("index {} out of bounds (len {})", i, list.len()))
        }
        (Value::Dict(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil)),
        // 字符语义（非字节）：与 `len()` 的 `chars().count()` 同一口径。
        (Value::String(s), Value::Int(i)) => {
            let i = checked_index(*i as f64, s.chars().count(), "string index")?;
            char_at(s, i)
        }
        (Value::String(s), Value::Float(n)) => {
            let i = checked_index(*n, s.chars().count(), "string index")?;
            char_at(s, i)
        }
        // v0.104.6：`{:?}` → 类型名 + Display。
        //
        // `Value::Dict` 是 `HashMap`，`Debug` 按迭代序打印，而 `RandomState`
        // 每进程随机 —— 于是**同一条错误信息在不同进程里键序不同**（本会话早前
        // 修过 Display / JSON / keys / values 的同类问题，`{:?}` 这条路漏了）。
        // 改用 `flow::type_name`（错误本来讲的就是「类型不对」）+ 已排序的
        // `Display`，顺带比整块 dump 更好读。
        _ => Err(format!(
            "cannot index {} with {}",
            crate::flow::type_name(obj),
            crate::flow::type_name(idx)
        )),
    }
}

/// 取字符串第 `i` 个**字符**（非字节）。越界报「字符数」而非「字节数」，
/// 否则与 `len(s)` 对不上 —— 那是 v0.104.6 之前 `len` 数字节时留下的错位。
fn char_at(s: &str, i: usize) -> Result<Value, String> {
    s.chars().nth(i).map(Value::Char).ok_or_else(|| {
        format!(
            "string index {} out of bounds (len {})",
            i,
            s.chars().count()
        )
    })
}

/// α.1: Value 转字符串（p"..." 拼接用，与 AST 解释器 evaluate_prompt 语义一致）
pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Char(c) => c.to_string(),
        other => format!("{:?}", other),
    }
}

// v0.75.83: vm::is_truthy 已删除 — 与 flow::is_truthy 语义分叉（本实现
// List/Dict 恒真，flow 版判非空；且 flow 版缺 Int 分支致 Int(0) 误判真）。
// 收敛为单一实现：crate::flow::is_truthy（MIR 条件分支唯一真值源），
// handlers 的 h_jump_if/h_jump_if_not 与 DAG Branch 共用。

/// α.2: 索引赋值 obj\[idx\] = val（就地修改）
///
/// v0.104.6：`List` 是**不可变**的，原地 `list[i] = v` 不再成立 ——
/// 值语义下「改」必须表达成「产出新列表并写回」。`List::set` 返回新列表，
/// 这里把新列表赋回 `*obj`（`*obj` 已是 `&mut Value`）。
pub fn index_assign_value(obj: &mut Value, idx: &Value, val: &Value) -> Result<(), String> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = checked_index(*i as f64, list.len(), "index")?;
            if i >= list.len() {
                Err(format!("index {} out of bounds (len {})", i, list.len()))
            } else {
                *list = list.set(i, val.clone());
                Ok(())
            }
        }
        (Value::List(list), Value::Float(n)) => {
            let i = checked_index(*n, list.len(), "index")?;
            if i >= list.len() {
                Err(format!("index {} out of bounds (len {})", i, list.len()))
            } else {
                *list = list.set(i, val.clone());
                Ok(())
            }
        }
        (Value::Dict(map), Value::String(key)) => {
            map.insert(key.clone(), val.clone());
            Ok(())
        }
        _ => Err("cannot index assign with given object and index".to_string()),
    }
}

/// α.2: MIR 模式匹配（简化版）
/// pat_str 来自 pattern_to_string 的序列化结果
pub fn self_match_pattern(
    val: &Value,
    pat_str: &str,
    _cond_reg: Option<&Value>,
    env: &mut Environment,
) -> bool {
    // 通配符匹配任意值
    if pat_str == "_" {
        return true;
    }
    // 变量模式：纯标识符（无 ":" 前缀）匹配任意值，绑定到 env
    if !pat_str.contains(':') {
        env.define(pat_str.to_string(), val.clone(), false);
        return true;
    }
    // nil 模式
    if pat_str == "nil" {
        return matches!(val, Value::Nil);
    }
    // bool 模式
    if pat_str == "bool:true" {
        return matches!(val, Value::Bool(true));
    }
    if pat_str == "bool:false" {
        return matches!(val, Value::Bool(false));
    }
    // int 模式: int:42
    if let Some(suffix) = pat_str.strip_prefix("int:")
        && let Value::Int(i) = val
        && let Ok(n) = suffix.parse::<i64>()
    {
        return i == &n;
    }
    // float 模式: float:3.14
    if let Some(suffix) = pat_str.strip_prefix("float:")
        && let Value::Float(f) = val
        && let Ok(n) = suffix.parse::<f64>()
    {
        return (f - n).abs() < FLOAT_PATTERN_EPSILON;
    }
    // v0.104.6 D317：bigint 模式: bigint:123
    //
    // `pattern_to_string`（`lower.rs:1255`）**会**把 `Literal::BigInt` 序列化成
    // `bigint:{n}`，但本函数此前**没有任何 `bigint:` 分支** ⇒ 该模式穿过全部
    // 分支后落到末尾的 `false`。
    //
    // 后果（实测）：`match 123n { 123n => "big", _ => "other" }` 恒得 `other`
    // —— **同类型、同值也不匹配**。这是序列化器与匹配器之间的**不对称**
    // （一端能产出、另一端认不出），不涉及任何语义取舍，故直接修。
    //
    // ⚠ **只修这一条，不碰 int↔float 互认**：`float:` 模式不认 `Value::Int`
    // （`match len("xy") { 2 => "two", _ => "other" }` 恒得 `other`，而
    // `len("xy") == 2` 为 `true`）是**另一件事** —— 那是「match 要不要跟
    // `eval_binary` 的 `==` 一样做数值提升」的**语言语义决定**，
    // 两种答案都自洽，故只报告不实施。见 CHANGELOG D317。
    if let Some(suffix) = pat_str.strip_prefix("bigint:")
        && let Value::BigInt(b) = val
        && let Ok(n) = suffix.parse::<num_bigint::BigInt>()
    {
        return b == &n;
    }
    // str 模式: str:hello
    if let Some(suffix) = pat_str.strip_prefix("str:")
        && let Value::String(s) = val
    {
        return s == suffix;
    }
    // v0.87: List vector pattern: list:vector:[p1,p2,...,rest] or list:vector:[p1,p2]
    if let Some(inner) = pat_str
        .strip_prefix("list:vector:[")
        .and_then(|s| s.strip_suffix(']'))
    {
        if let Value::List(items) = val {
            // Parse elements and detect rest marker {..}
            let parts: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').collect()
            };
            // Find rest marker ..rest_name (pattern_to_string serializes without braces)
            let rest_pos = parts.iter().position(|p| p.starts_with(".."));
            match rest_pos {
                Some(pos) => {
                    // Elements before rest
                    if items.len() < pos {
                        return false;
                    }
                    // Match prefix elements
                    for (i, (item, pat)) in items.iter().zip(parts.iter()).enumerate() {
                        if i == pos {
                            break;
                        }
                        if !self_match_pattern(item, pat, None, env) {
                            return false;
                        }
                    }
                    // Parse rest variable name from ..rest_name
                    let rest_name = parts[pos].strip_prefix("..").unwrap_or(parts[pos]);
                    if !rest_name.is_empty() {
                        // Bind the remaining items as a list to the rest variable
                        env.define(
                            rest_name.to_string(),
                            Value::List(items.slice(pos, items.len())),
                            false,
                        );
                    }
                    return true;
                }
                None => {
                    // No rest — exact length match
                    if items.len() != parts.len() {
                        return false;
                    }
                    return items
                        .iter()
                        .zip(parts.iter())
                        .all(|(item, pat)| self_match_pattern(item, pat, None, env));
                }
            }
        }
        return false;
    }
    // 列表模式: list:[p1,p2,...] (cons-list format, legacy)
    if pat_str == "list:[]" {
        return matches!(val, Value::List(items) if items.is_empty());
    }
    if let Some(inner) = pat_str
        .strip_prefix("list:[")
        .and_then(|s| s.strip_suffix(']'))
    {
        // Skip cons-list with | separator (legacy)
        if inner.contains('|') {
            return false;
        }
        if let Value::List(items) = val {
            let pats: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').collect()
            };
            if items.len() != pats.len() {
                return false;
            }
            return items
                .iter()
                .zip(pats.iter())
                .all(|(item, pat)| self_match_pattern(item, pat, None, env));
        }
        return false;
    }
    // 字典模式: dict:{k1:v1,k2:v2,...}
    if pat_str == "dict:{}" {
        return matches!(val, Value::Dict(map) if map.is_empty());
    }
    if let Some(inner) = pat_str
        .strip_prefix("dict:{")
        .and_then(|s| s.strip_suffix('}'))
    {
        if let Value::Dict(map) = val {
            let pairs: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').collect()
            };
            // Handle trailing .. (rest marker)
            let pairs = if let Some(pos) = pairs.iter().position(|p| p.trim() == "..") {
                &pairs[..pos]
            } else {
                &pairs[..]
            };
            return pairs.iter().all(|pair| {
                if let Some((k, v)) = pair.split_once(':') {
                    map.get(k)
                        .map(|item| self_match_pattern(item, v, None, env))
                        .unwrap_or(false)
                } else {
                    false
                }
            });
        }
        return false;
    }
    // 守卫模式: guard:inner — 只检查内部模式匹配
    if let Some(inner) = pat_str.strip_prefix("guard:") {
        return self_match_pattern(val, inner, None, env);
    }
    // 字符模式: char:c
    if let Some(suffix) = pat_str.strip_prefix("char:")
        && let Value::Char(c) = val
    {
        return *c == suffix.chars().next().unwrap_or('\0');
    }
    // 未匹配的模式
    false
}

/// α.3: 查找并执行 main task（与 AST 路径的 interpret() 末尾逻辑一致）。
/// 扫描 func.body 中的 TaskDef，找到 name="main" 且无参的，执行其 body。
/// 若不存在或非无参 main 则静默返回 Ok。
///
/// v0.75.9: 接收 `&Arc<MirFunction>`；TaskDef body 克隆进 Arc 以便走
/// `execute` 的全局 DAG 缓存。
pub fn run_main_task(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let mut main_body: Option<Arc<MirFunction>> = None;
    for inst in &func.body {
        if let MirInst::TaskDef { name, params, body } = inst
            && name == "main"
            && params.is_empty()
        {
            main_body = Some(Arc::new((**body).clone()));
            break;
        }
    }
    if let Some(main_func) = main_body {
        let mut main_env = env.clone();
        let _ = execute(&main_func, interp, &mut main_env, effects)?;
    }
    Ok(())
}

/// α.10: 控制流信号（与 AST `FlowSignal` 同形）。
/// REPL 与 differential test 用来观察 Return/Break/Continue 出口；
/// 主路径 `execute` 直接返回 `Result<Value, String>`，不暴露信号。
#[derive(Debug, Clone, PartialEq)]
pub enum MirSignal {
    /// 正常落到函数末尾，无显式 return。
    None,
    /// 显式 `return value`。
    Return(Value),
    /// `break label`。
    Break,
    /// `continue label`。
    Continue,
    /// v0.70: Vote to halt — agent signals completion. In BSP context
    /// marks the vertex as Halted. Equivalent to Return in linear context.
    Halt(Option<Value>),
}

/// α.10: 与 `execute` 等价，但返回 `(MirSignal, Value)`，保留信号。
/// REPL/差分测试用此观测控制流出口；生产路径仍走 `execute`。
///
/// v0.104.6 D467：`run_mir_with_signal` 与 `run_mir_with_signal_cached` 已**迁入
/// [`engine`]**（它们是引擎入口，不是壳层工具），改名
/// [`engine::run_with_signal`] / [`engine::run_with_signal_cached`]。
/// 本文件不再持有任何执行入口的实现或转发。
/// α.10: `run_main_task` 的信号感知变体。
/// main task 中允许出现显式 `return value`——返回它的值；否则返回 Value::Nil。
pub fn run_main_task_with_signal(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(MirSignal, Value), String> {
    let mut main_body: Option<Arc<MirFunction>> = None;
    for inst in &func.body {
        if let MirInst::TaskDef { name, params, body } = inst
            && name == "main"
            && params.is_empty()
        {
            main_body = Some(Arc::new((**body).clone()));
            break;
        }
    }
    let Some(main_func) = main_body else {
        return Ok((MirSignal::None, Value::Nil));
    };
    let mut main_env = env.clone();
    let value = execute(&main_func, interp, &mut main_env, effects)?;
    Ok((MirSignal::Return(value.clone()), value))
}

// ===================================================================

// v0.104.6 D467：执行引擎是本模块的**唯一**执行实现，独立成 `engine`
// （原名 `dag`，因其就是 DAG/BSP 调度器）。
//
// 此前此处是 `mod dag; pub use dag::*;` —— 把子模块内容**平铺**回壳层，
// 于是 `vm::run_dag*` 与（壳里的）`vm::run_mir*` 在同一个命名空间里看不出
// 谁转发给谁。现改为显式：引擎公开，执行入口由它转出。
pub mod engine;

// 显式转出（而非 `pub use engine::*`）：只暴露**真实调用过**的引擎入口，
// 保留壳层作为「模块入口」的语义边界。
//
// v0.104.6 D467：`run_with_signal*`（原 `run_mir_with_signal*`）已迁入 engine，
// 在此一并转出，使既有 `vm::run_with_signal` 调用路径保持可用。
pub use engine::{
    DagExecMemo, execute_program, run_dag, run_dag_with_signal, run_dag_with_signal_memo,
    run_with_signal, run_with_signal_cached,
};

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::interpreter::Interpreter;
    use crate::mir::{MirFunction, MirInst};
    use crate::value::Value;

    fn run(body: Vec<MirInst>) -> Result<Value, String> {
        let n_regs = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(1);
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
            ..Default::default()
        };
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        // v0.75.9: 包裹 Arc（execute_program 签名变更）
        execute_program(
            &Arc::new(func),
            &mut interp,
            &mut env,
            &mut crate::mir::effect::Effects::new(),
        )
    }

    #[test]
    fn dag_exec_const() {
        assert_eq!(
            run(vec![MirInst::Const(0, Value::Int(42))]).unwrap(),
            Value::Int(42)
        );
    }

    #[test]
    fn dag_exec_binary_add() {
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(10)),
                MirInst::Const(1, Value::Int(32)),
                MirInst::BinaryOp(2, 0, BinaryOp::Add, 1)
            ])
            .unwrap(),
            Value::Int(42)
        );
    }

    #[test]
    fn dag_exec_chain() {
        // 注：`execute_program` 经 `MirHost::dag_cache()` 取 DAG，即
        // `dag_analyze → dag_optimize → prune_sequence_edges` —— **与生产链同链**。
        //
        // ⚠ 此前这里写的是「直调 `dag_analyze`、**不跑 `dag_optimize`**」，
        // 那是**旧实现**的形状：v0.104.6 改用宿主缓存后已不成立（与
        // `vm/engine.rs::execute_program` 的说明矛盾，以后者为准）。
        //
        // 当年的担心是「常量折叠把 `BinaryOp(4, 2, Add, 3)` 折成追加在尾部的
        // 新节点，取值落到前一个节点（得 3，应 6）」—— 现状实测仍为 6，
        // 本条即其护栏。
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Int(2)),
                MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
                MirInst::Const(3, Value::Int(3)),
                MirInst::BinaryOp(4, 2, BinaryOp::Add, 3)
            ])
            .unwrap(),
            Value::Int(6)
        );
    }

    #[test]
    fn dag_exec_list() {
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Int(2)),
                MirInst::ListLit(2, vec![0, 1])
            ])
            .unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2)].into())
        );
    }

    // ─── v0.75.10: 寄存器级增量（DagExecMemo）─────────────────────────

    /// 同一 memo 连跑两次同一 body：第一次全量，第二次复用记忆。
    /// 返回两次结果 + 最终 memo。
    fn run_memo_twice(body: Vec<MirInst>) -> (Value, Value, DagExecMemo) {
        let n_regs = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(1);
        let func = MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
            ..Default::default()
        };
        let dag = crate::mir::dag::dag_analyze(&func);
        let mut memo = DagExecMemo::new();
        let run = |memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            run_dag_with_signal_memo(
                &dag,
                &func,
                memo,
                &mut interp,
                &mut env,
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("memo run should succeed")
            .1
        };
        let v1 = run(&mut memo);
        let v2 = run(&mut memo);
        (v1, v2, memo)
    }

    /// 第二次跑（env 空、regs 重建）时，纯节点输入与上次相等 → 全部跳过。
    /// 结果不变（记忆化不改变语义），且第二次无实际执行。
    #[test]
    fn memo_second_run_skips_pure_nodes() {
        let (v1, v2, memo) = run_memo_twice(vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        assert_eq!(v1, Value::Int(3));
        assert_eq!(v2, Value::Int(3), "记忆化不得改变结果");
        assert_eq!(memo.executed_nodes, 3, "第一次全量执行");
        assert_eq!(memo.skipped_nodes, 3, "第二次纯节点全部跳过");
    }

    /// 输入变化 → 不跳过（重执行），记忆仍正确。
    /// 用 Var 重建 regs（Var 非纯，永远重跑）驱动 BinaryOp 输入变化。
    #[test]
    fn memo_input_change_forces_recompute() {
        let n_regs = 3;
        let body = vec![
            MirInst::Var(0, "a".to_string()),
            MirInst::Const(1, Value::Int(10)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ];
        let dag = crate::mir::dag::dag_analyze(&MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
            ..Default::default()
        });
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
            ..Default::default()
        };
        let mut memo = DagExecMemo::new();
        let run = |env_val: i64, memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            env.define("a".to_string(), Value::Int(env_val), false);
            run_dag_with_signal_memo(
                &dag,
                &func,
                memo,
                &mut interp,
                &mut env,
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("memo run should succeed")
            .1
        };
        assert_eq!(run(1, &mut memo), Value::Int(11));
        assert_eq!(run(1, &mut memo), Value::Int(11), "a 未变 → BinaryOp 跳过");
        // run2：Var 重跑（非纯），Const（输入为空，未变）与 BinaryOp（输入
        // (1,10) 与记录相等）跳过 → skipped=2。
        assert_eq!(memo.skipped_nodes, 2, "第二次 Const + BinaryOp 跳过");
        assert_eq!(run(5, &mut memo), Value::Int(15), "a 变 → BinaryOp 重算");
        // run3：Var 重跑，Const 再跳过，BinaryOp 输入 (5,10) ≠ 记录 (1,10) → 重算。
        // skipped 累计 = 2 + 1（Const）= 3。
        assert_eq!(memo.skipped_nodes, 3);
    }

    /// 纯白名单不含 env 读取：含 Var 的程序第二次跑不被全跳（Var 重跑）。
    #[test]
    fn memo_var_not_skipped() {
        let n_regs = 2;
        let body = vec![MirInst::Var(0, "x".to_string())];
        let dag = crate::mir::dag::dag_analyze(&MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
            ..Default::default()
        });
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
            ..Default::default()
        };
        let mut memo = DagExecMemo::new();
        let run = |memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            env.define("x".to_string(), Value::Int(7), false);
            run_dag_with_signal_memo(
                &dag,
                &func,
                memo,
                &mut interp,
                &mut env,
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("memo run should succeed")
            .1
        };
        assert_eq!(run(&mut memo), Value::Int(7));
        assert_eq!(run(&mut memo), Value::Int(7));
        // Var 非纯（env 读取不可记忆）：永不 record（executed_nodes 只统计纯节点），
        // 也永不跳过。
        assert_eq!(memo.executed_nodes, 0, "Var 不在纯白名单，不记 memo");
        assert_eq!(memo.skipped_nodes, 0, "Var 每次重跑，无跳过");
    }
}

//! v0.104.6 回归护栏：**嵌套循环**中的 `break` / `continue` 标签解析。
//!
//! ## 缺陷背景
//!
//! 循环出口标签要到 body 全部 emit 之后才知道，历史上靠「body 结束后扫
//! `body_start..body_end` 全区间重写标签」回填（`src/parser_v3/emit.rs` 与
//! `src/mir/lower.rs` 各一份）。该区间在**嵌套循环**时包含内层循环的全部
//! 指令 —— 外层回填会把内层已正确指向「内层出口 / 内层增量」的标签覆盖成
//! 外层的：
//!
//! - 内层 `break`    → 被改成外层出口 → 内层 break 跳出了外层循环；
//! - 内层 `continue` → 被改成外层 for 的增量位置 → 控制流失控，
//!   **整个程序静默结束：无任何输出、退出码 0**。
//!
//! 修复：`EmitContext` 增加与 `loop_stack` 平行的 `break_slots` /
//! `continue_slots`，emit 时按作用域登记索引，循环退出时只回填本层登记过的
//! 索引，嵌套关系由栈天然隔离。
//!
//! ## 为什么要有这个文件
//!
//! 修复前这批用例全部无法通过，而既有测试套件（972 个 lib 测试 + 各集成组）
//! **一个都没覆盖到** —— 这正是该缺陷能长期存活的原因。本文件锁死该语义。

mod e2e_helpers;

use e2e_helpers::assert_source_ok;
use mora::value::Value;

fn num(v: Value) -> f64 {
    match v {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    }
}

/// 嵌套 for + 内层 `break`：外层必须跑满 3 轮，内层每轮只执行到 break 前。
///
/// 修复前：`outer=1, inner=1`（内层 break 跳到了外层出口）。
#[test]
fn nested_for_inner_break_exits_only_inner() {
    let v = assert_source_ok(
        "let outer = 0\n\
         let inner = 0\n\
         for x in [1, 2, 3]\n  \
           assign outer = outer + 1\n  \
           for y in [1, 2, 3]\n    \
             if y == 2 then\n      \
               break\n    \
             end\n    \
             assign inner = inner + 1\n  \
           end\n\
         end\n\
         outer * 100 + inner",
    );
    assert_eq!(
        num(v),
        303.0,
        "嵌套 for 的内层 break 只能跳出内层：outer=3 inner=3"
    );
}

/// 嵌套 for + 内层 `continue`：外层跑满 3 轮，内层每轮跳过 y==2 计 2 次。
///
/// 修复前：程序**静默终止**，首行 print 都不执行，退出码 0。
#[test]
fn nested_for_inner_continue_completes_program() {
    let v = assert_source_ok(
        "let outer = 0\n\
         let inner = 0\n\
         for x in [1, 2, 3]\n  \
           assign outer = outer + 1\n  \
           for y in [1, 2, 3]\n    \
             if y == 2 then\n      \
               continue\n    \
             end\n    \
             assign inner = inner + 1\n  \
           end\n\
         end\n\
         outer * 100 + inner",
    );
    assert_eq!(
        num(v),
        306.0,
        "嵌套 for 的内层 continue 不得吞掉后续执行：outer=3 inner=6"
    );
}

/// 嵌套 while + 内层 `break`：内层每轮在 b==2 跳出，外层 3 轮各进 2 次。
#[test]
fn nested_while_inner_break_exits_only_inner() {
    let v = assert_source_ok(
        "let a = 0i\n\
         let total = 0i\n\
         while a < 3i\n  \
           let b = 0i\n  \
           while b < 10i\n    \
             if b == 2i then\n      \
               break\n    \
             end\n    \
             assign total = total + 1i\n    \
             assign b = b + 1i\n  \
           end\n  \
           assign a = a + 1i\n\
         end\n\
         total",
    );
    assert_eq!(num(v), 6.0, "嵌套 while 的内层 break 只能跳出内层");
}

/// 三层嵌套 + 最内层 `break`：`z` 在 2 处跳出（assign 在 break 之前，故
/// z=1、z=2 各计一次），`2 * 2 * 2 = 8`。
///
/// v0.104.6 已修复并转正：根因是 CSE 把「if 块里的 `Const(1)`」与「if 之后
/// 循环体增量用的 `Const(1)`」合并，制造了跨互斥控制区域的虚假数据依赖
/// （见 `CseDagRule::rewrite` 的 D9 注释）。
#[test]
fn triple_nested_innermost_break() {
    let v = assert_source_ok(
        "let n = 0\n  \
         for x in [1, 2]\n    \
           for y in [1, 2]\n      \
             for z in [1, 2, 3]\n        \
               assign n = n + 1\n        \
               if z == 2 then\n          \
                 break\n        \
               end\n      \
             end\n    \
           end\n  \
         end\n\
         n",
    );
    assert_eq!(num(v), 8.0, "三层嵌套最内层 break：2*2*2=8");
}

/// `break` 的目标语义随**语句顺序**变化，两者都必须正确。
///
/// 修复前两者的行为差异曾被我误判为缺陷；这里固化正确的期望值，
/// 防止再次把「语义本就依赖顺序」误读成 bug。
#[test]
fn break_target_depends_on_statement_order() {
    // 判 break 在前：break 那轮不执行 assign → 计 2
    let a = assert_source_ok(
        "let n = 0\n\
         for x in [1, 2, 3, 4, 5]\n  \
           if x == 3 then\n    \
             break\n  \
           end\n  \
           assign n = n + 1\n\
         end\n\
         n",
    );
    assert_eq!(num(a), 2.0, "break 检查在前：break 那轮不计数");

    // assign 在前：break 那轮先计数 → 计 3
    let b = assert_source_ok(
        "let n = 0\n\
         for x in [1, 2, 3, 4, 5]\n  \
           assign n = n + 1\n  \
           if x == 3 then\n    \
             break\n  \
           end\n\
         end\n\
         n",
    );
    assert_eq!(num(b), 3.0, "assign 在前：break 那轮已计数");
}

/// 单层循环语义不得被嵌套修复波及（回归方向相反的护栏）。
#[test]
fn single_level_control_flow_unchanged() {
    assert_eq!(
        num(assert_source_ok(
            "let n = 0\n\
             for x in [1, 2, 3, 4, 5]\n  \
               if x == 3 then\n    \
                 break\n  \
               end\n  \
               assign n = n + 1\n\
             end\n\
             n"
        )),
        2.0
    );
    // for + continue（1+2+4+5 = 12）
    assert_eq!(
        num(assert_source_ok(
            "let n = 0\n\
             for x in [1, 2, 3, 4, 5]\n  \
               if x == 3 then\n    \
                 continue\n  \
               end\n  \
               assign n = n + x\n\
             end\n\
             n"
        )),
        12.0
    );
    // while + break
    assert_eq!(
        num(assert_source_ok(
            "let n = 0i\n\
             let k = 0i\n\
             while k < 50i\n  \
               if k == 4i then\n    \
                 break\n  \
               end\n  \
               assign n = n + 1i\n  \
               assign k = k + 1i\n\
             end\n\
             n"
        )),
        4.0
    );
    // 循环之后的语句必须照常执行
    //
    // 形态说明：这里在两层的循环体里各放一条 `assign`，不只是为了断言值，
    // 也是为了避开一处**既有的 typeck 限制** —— `if <loopvar> == n` 紧跟
    // `for` 头部（内层 body 的第一条语句）时报 `Unbound variable`；循环体
    // 内先有别的语句则正常。该限制与本次修复无关，pristine HEAD 同样如此。
    assert_eq!(
        num(assert_source_ok(
            "let a = 0\n\
             for x in [1, 2, 3]\n  \
               assign a = a + 1\n  \
               for y in [1, 2, 3]\n    \
                 if y == 2 then\n      \
                   break\n    \
                 end\n    \
                 assign a = a + 10\n  \
               end\n\
             end\n\
             a"
        )),
        33.0,
        "嵌套循环跑满且循环之后的语句必须执行（outer 3 + inner 3x10）"
    );
}

/// `while` + `continue`（D9 主复现件）：修复前返回 0，应为 4。
///
/// v0.104.6 已修复并转正。根因：`dag_optimize` 的 CSE 把 if 块内的
/// `Const(1)`（`k = k + 1` 用）与 if 之后循环体增量的 `Const(1)` 合并，
/// 使 `n = n + 1` 在数据上依赖 if 块；k != 2 时 if 块不执行 → 该常量永不
/// 写入 → `n = n + 1` 永久 not-ready → 循环体尾部不执行 → 静默返回 0。
/// 修复：`CseDagRule` 要求胜者与败者**同基本块**（沿 Sequence 边可达）。
#[test]
fn while_continue_bare_expression() {
    assert_eq!(
        num(assert_source_ok(
            "let n = 0i\n\
             let k = 0i\n\
             while k < 5i\n  \
               if k == 2i then\n    \
                 assign k = k + 1i\n    \
                 continue\n  \
               end\n  \
               assign n = n + 1i\n  \
               assign k = k + 1i\n\
             end\n\
             n"
        )),
        4.0
    );
}

// ============================================================================
// D9 修复记录（v0.104.6）
// ============================================================================
//
// **现象**：`while` + `continue` 的程序只跑一轮，返回 0（应 4）；三层嵌套 +
// 最内层 `break` 返回 Nil（应 8）。「尾部是否追加一条 `print`」会翻转结果。
//
// **范围**（路径对照实验确认）：`while`+`continue` 坏；`while` 无跳转、
// `while`+`break`、`for`+`break` 均正确。`ParserV3::compile` 与
// `compile_and_opt` 两条路径结果**一致**（此前「路径差异」的猜测已被证伪），
// 故与编译路径无关。DAG 结构本身经逐节点核对是**正确**的（手工模拟执行链：
// k=0→1→2(continue 跳回 k=3)→3→4→5→退出，n 应为 4）。
//
// **根因**：`dag_optimize` 的 CSE（`CseDagRule`）把 **if 块内**的
// `Const(1)`（`k = k + 1` 用）与 **if 之后**循环体增量的 `Const(1)` 判为
// 等价并合并，随后用 `reg_rename` 把败者 `dst` 的全部消费者改写到胜者 `dst`。
// 两个寄存器都是单定义，`is_control_target` / `is_multi_defined` 两道既有
// 守卫全部放行 —— 但两块**互斥**：k != 2 时 if 块不执行，其 `Const(1)` 永不
// 写入该寄存器 → `n = n + 1` 永久 not-ready → 循环体尾部永不执行 → 循环
// 无法推进 → `run_dag_with_signal_memo` 的 `ready.is_empty()` 兜底分支清空
// 前沿 → **静默结束**（无报错，退出码 0）。
//
// 这是 CSE 的经典误编译：**跨互斥控制流区域合并节点，制造虚假数据依赖**。
// v0.103 曾为相关症状（`while...if...break` 死循环）打过补丁，但只限制了
// Sequence 边的缝合范围，**没有约束「胜者是否在败者执行时必然已执行」**。
//
// **修复**：`CseDagRule` 新增支配性判据 —— 胜者与败者必须**同基本块**
// （沿 Sequence 边可达；Sequence 边只在块内创建，故二者等价）。同块节点
// 同生共死，合并必然安全。这是**充分**条件（真正支配但跨块的节点会被保守
// 地放弃合并），代价只是少做少量本可做的优化。
//
// **D9 第二处（entry 死块）— v0.104.6 已修**：
//
// 现象：`while`+`continue` 只跑一轮返回 0、三层嵌套+`break` 返回 Nil；
// 更隐蔽的是「尾部是否追加一条 `print`」会翻转结果 —— 旧公式把死块拉回
// 入口集、让它执行、顺带把 `executed[]` 置位，尾语句才解锁，是巧合而非设计。
//
// 真正阻塞点（依次定位，三处缺一不可）：
//
// 1. **`Label` 节点孤立** —— 节点创建时 Sequence 链刻意跳过 Label，Label
//    自身**没有任何出边**。`--opt=1` 的 SSA 在 pc 0 插入 `Label(0)`，边表里
//    没有 `0 -> 1` → 可达集 = `{0}`，入口只剩这个 no-op → 顶层结果静默变 Nil
//    （`mir_ssa_roundtrip` 的 `top_level_*_equiv` 三项）。修：给每个 Label 补
//    一条到其后第一个非 Label 节点的控制边。
// 2. **删节点破坏可达性** —— `apply_rewrite` 剥离 removed 节点的边，只用 4a
//    的 Sequence 缝合补回「同分量内」的前后继，后继激活路径断裂。修：removed
//    节点改**透明穿通**（不剥离其边，`Removed` 分支 no-op 但仍沿原出边传播）。
// 3. **死块留在 Sequence 链上** —— 就绪门槛是
//    `seq_preds[n].iter().all(|&p| executed[p])`，而尾语句的 Sequence 前驱
//    恰在死块内（常量折叠删掉 `JumpIfNot` 后 else 臂成死块）。修：执行器构造
//    `seq_preds` 时**只收录可达的前驱**（`dag.reachable`，由 `dag_analyze` 算出、
//    优化阶段保持有效）。
//
// 另有一处配套：规则「新增节点 + 标旧节点 Removed」时，追加节点必须**继承**被
// 替换节点的可达性（`dag.reachable` 按当时的 `nodes.len()` 分配，追加下标落在
// 数组外等同 `false`）—— 否则追加节点不激活、其写出的寄存器永不 ready。
//
// 三处齐备后，`dag_analyze` 自 v0.104.2 起用、却被 `dag_optimize` /
// `prune_sequence_edges` 旧公式悄悄撤销的「可达 ∧ 无入边」入口过滤才得以恢复。
//
// **D10（9 层管线在裸 `if` 上分叉）— v0.104.6 已修**：
//
// `fcfg_lower` 不物化隐式 else 块，而 `emit.rs` 物化（`Jump / Const(Nil) /
// Copy(dst,nil)`）。两者运行结果等价，但 9 层差分按指令类别逐条比对 → 凡含
// 裸 `if` 的程序差分必失败 → 管线静默回落，执行器切换被阻塞。
//
// 此前修复它会引发 6 项回归（`e2e_if_then_block_and_single_stmt_forms` 等），
// 当时判定「差分只比类别、通过 ≠ 语义正确、管线需先补自身缺陷」。**该判断是
// 错的**：那 6 项回归的根因全在 DAG 侧（即上面 D9 第二处的 1/2/3），管线本身
// 无辜。DAG 侧修好后，物化隐式 else 即安全 —— 实测 8 类程序（裸 if 顶层 /
// 循环内 / if-else / break / continue / 嵌套 / 值形态 / else-if 链）差分
// **全部通过**，管线路由首次真正生效。

/// v0.104.6 回归护栏：`if` 作**值**（`let x = if … then 5 end`）不能被死分支饿死。
///
/// 缺陷：常量条件被 `IfSimplifyRule` 折叠掉 `JumpIfNot` 后，else 臂成为**死块**，
/// 其中的 `Const(Nil)` 却与后续活代码同处一条 Sequence 链上。CSE 按「值相等」
/// 把活代码的 `Const` 合并进这个**死块**里的等价节点，并 `reg_rename` 把消费者
/// 改写成读死块的寄存器 —— 那个寄存器永远 not-ready → 消费者永不执行 → 其后
/// 整条尾部（包括 `print`）静默消失，退出码 0。
///
/// 修：`CseDagRule` 增加「胜者必须与败者一样可达」判据（见 dag_rule.rs）。
/// 注：仅靠此前的「同基本块」判据挡不住 —— Sequence 边会从死块跨进活块。
///
/// 本用例走 `compile_and_opt`（CLI 同款链路）：**只有它会跑 `apply_rules`**，
/// 而折叠正是死块产生的源头。`e2e_helpers::run_source` 走裸
/// `ParserV3::compile`、不跑 `apply_rules`，在该路径下末表达式求值另有既有限制
/// （pristine HEAD 同样返回 Nil），与本缺陷无关，故不纳入断言。
#[test]
fn if_as_value_not_starved_by_dead_branch() {
    use std::sync::Arc;
    let run = |src: &str| -> Value {
        let (f, _w) = mora::cli::compile_and_opt(src, None).expect("compile");
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let arc = Arc::new(f);
        mora::mir::vm::run_mir(
            &arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )
        .expect("run_mir")
    };

    // 条件是常量 → else 臂折叠成死块（本缺陷的触发形态）
    assert_eq!(
        num(run("let x = if 1 == 1 then 5 end\nx")),
        5.0,
        "if 作值：常量条件折叠（产生死块）后仍须取到 then 分支的值"
    );
    assert_eq!(
        num(run("let x = if 1 == 1 then 5 end\nx + 1")),
        6.0,
        "if 作值：结果被后续表达式消费，尾部必须照常执行"
    );
    // 条件是变量 → 无死块，两种分支都应正确
    assert_eq!(
        num(run("let c = 1\nlet x = if c == 1 then 5 else 7 end\nx")),
        5.0,
        "if/else 作值：取 then 分支"
    );
    assert_eq!(
        num(run("let c = 2\nlet x = if c == 1 then 5 else 7 end\nx")),
        7.0,
        "if/else 作值：取 else 分支"
    );
}

// ============================================================================
// 已知未修缺陷（定位完成，修复方案均破坏正确性）
// ============================================================================
//
// **E1：`if/else` 之后紧跟表达式时，返回值是 `x` 而非 `x + 1`。**
//
// 复现：`let c = 1 / let x = if c == 1 then 5 else 7 end / x + 1` → 得 5.0（应 6.0）；
// `c = 2` 的 else 分支正常（得 8.0）—— 两条路径不对称。
//
// 机制：`dag_analyze` 把 else 臂与汇合点串成 `12→13→14→15…` 同一个 Sequence 连通
// 分量，于是 `Define("x", …)` 的 Sequence 前驱是 else 臂的 `Copy`；而 `c == 1`
// 成立时 else 臂**不执行**，就绪门槛 `seq_preds[n].all(|&p| executed[p])` 永远
// 不满足 → 汇合点不就绪 → 其后 `x + 1` 从不求值，`run_mir` 返回 `x` 的值。
// 无报错、退出码 0。
//
// 试过三种更严的 `seq_preds` 判据，**都破坏正确性**（后两种都挂
// `e2e_loop_bodies_are_typechecked` 与 `semantics_control_flow_runs_via_mir`）：
//   1. 「同 Sequence 分量」—— **无效**：else 臂与汇合点本就在同一分量内。
//   2. 「分量单入口才门控」—— **太粗**：多入口分量被**整体**关掉门控，连汇合点
//      之后真正的直线段（15→16→17…）的块内保序也一起丢了。
//   3. **全图支配**（照 `ssa.rs::compute_dominators` 移植一份到 `MirDag`，
//      多入口按虚根处理）—— 原理上最正，**手工验证 5/5 全对**（then/else 分支、
//      裸 if 作值、链式 let、循环体内 let），但仍挂那两个循环测试。
//
// **第 3 次失败给出一个值得记住的反例**：支配关系在**有环图**上比执行顺序
// 更弱。循环体内 `Dom(B) = {header, B}` —— 因为存在 `header→B` 这条绕开 A
// 的边，A **不支配** B，支配关系恰好丢掉了循环体真正依赖的「块内先后」信息。
// 故在这个执行器里：**无环汇合点上「线性」是非线性的特例，循环体上不是** ——
// 「线性」那一路携带了支配关系会丢弃的信息，两者**不构成子集关系**。
//
// 正确判据应是**分量内的局部支配**：「P 位于从该分量**每个入口**到 N 的
// **每条**路径上」才构成保序约束。无环汇合点（入口 {12,14}）下 node 13 不满足
// → 不门控 → E1 修复；单入口的循环体满足 → 块内保序保住。它与上面第 2 次的
// 关键差别：**只剔除不构成约束的那些前驱，而非对整个分量关门控**。
// 尚未实现 —— 需要在分量内做「必现节点」传播，且循环回边会让必现集在单轮内
// 不稳定，必须按不动点迭代。
//
// `DeadNodeDagRule` 以「有出边」判死活，而 removed 节点的边现在**被保留**
// （`dag.reachable` 的有效性依赖它们），故「消费方全是 Removed」的节点虽已死
// 却删不掉，会逐轮堆积（每轮参与 `node_ready` 判定、在 `seq_preds` 占位）。
// 试过改判据为「有存活出边」，破坏 `jit_equiv_folded_constants` 与
// `if_as_value_not_starved_by_dead_branch` —— 沿穿通链继续承担激活传递的节点
// 也被判死并摘掉，链条断裂。两者相较，**正确性优先**：保留旧判据，接受这一
// 有界开销。

/// D11：循环变量必须可用于**循环体首条语句**。
///
/// v0.104.6 修复前，`WitnessKind::Loop` 的 typeck 预扫直接递归 body 而**不
/// 登记循环变量**（`Closure`/`FnDef` 都先登记形参）。于是
/// `if <loopvar> == n` 作为循环体第一条语句报
/// `Unbound variable`；在它前面加任何一条别的语句即正常。
#[test]
fn loop_var_usable_in_first_body_statement() {
    // 外层 for：if 紧跟 for 头部
    let v = assert_source_ok(
        "let c = 0\n\
         for x in [1, 2, 3]\n  \
           if x == 2 then\n    \
             assign c = c + 1\n  \
           end\n\
         end\n\
         c",
    );
    assert_eq!(num(v), 1.0, "if 紧跟 for 头部应可用循环变量");

    // 内层 for：同一形态在嵌套中同样成立
    let v2 = assert_source_ok(
        "let c = 0\n\
         for x in [1, 2]\n  \
           for y in [1, 2, 3]\n    \
             if y == 2 then\n      \
               assign c = c + 1\n    \
             end\n  \
         end\n\
         end\n\
         c",
    );
    assert_eq!(num(v2), 2.0, "内层 for 首条语句同样可用循环变量");

    // 循环变量参与算术
    let v3 = assert_source_ok(
        "let s = 0\n\
         for n in [1, 2, 3]\n  \
           assign s = s + n * 2\n\
         end\n\
         s",
    );
    assert_eq!(num(v3), 12.0, "循环变量参与算术");
}

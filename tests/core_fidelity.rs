//! v0.104.6 D481：Core 层的**保真度**判据 —— 产物里该看见的东西，不能丢。
//!
//! ## 背景
//!
//! Core 层是**影子 IR**（产出不进执行路径，`mir/pipeline.rs`），但它是未来
//! IR 的设计稿 ⇒ 产物里**说谎**比「不完整」更糟。
//!
//! D481 修掉的「说谎」清单：
//!
//! | 构造 | D481 之前 | 现在 |
//! |---|---|---|
//! | `match` 分支的**绑定名** | 恒为空 ⇒ `x when x > 0` 的 `x` 根本不存在 | 从模式里取出 |
//! | `match` 分支的**变体名** | `format!("{:?}", pattern)` ⇒ 连 `Span` 都进去了 | 只留可读形式 |
//! | `break` / `continue` | `CoreInst::Jump(0)` ⇒ 一条**假边**（目标是块 0） | 真的终结跳转 |
//! | 管道 `\|>` | `Const(dst, Nil)` ⇒ 两端全丢 | `Call(dst, left, [right])` |
//! | 闭包的**捕获变量** | 恒为空，且注释谎称「由 EHIR 分析确定」 | 过近似 + 注释说真话 |
//! | 区域构造（`parallel` / `observe` / `span` / `worker` / `transaction` / `with` / `solve` / `section` / `export`） | 原地摊平 ⇒ 构造**完全不可见** | `CoreInst::Scope`（体是独立嵌套代码区，形状镜像 MIR 层） |
//!
//! D482 让 Core 层**可执行**（有了自己的执行器 + 与 MIR 的语义对拍）⇒ 顺带
//! 发现 D481 登记的「有损清单」里，**模型**这一层是可以补齐的：D483 给
//! `eval` / `aggregate` / quasiquote / `commit` / `rollback` / `orchestrate`
//! 各补了一个原语，载荷原样带过 ⇒ 「有损登记」表**清空**。
//!
//! | 构造 | D482 之前 | D483 之后 |
//! |---|---|---|
//! | `eval` | `Const(dst, Nil)`（断言名 / expects 全丢，且**覆盖**了值寄存器） | `CoreInst::Eval { name, given, expects, tolerance, replay_path }` |
//! | `aggregate` | `Const(dst, Nil)`（聚合器名全丢，且**覆盖**了值寄存器） | `CoreInst::Aggregate { name, value }` |
//! | quasiquote | `Const(dst, Nil)`（片段全丢） | `CoreInst::Quasiquote { dst, segments }` |
//! | `commit` / `rollback` | `Const(dst, Nil)`（与「别的语句返回 Nil」**无法区分**） | `CoreInst::Commit` / `CoreInst::Rollback` |
//! | `orchestrate` | `Scope{kind:"orchestrate"}`（**空**区域，载荷全丢） | `CoreInst::Orchestrate { input_var, result_var, kind }`（载荷原样） |
//!
//! ## 判据
//!
//! ① `match` 的 arm 保留绑定名、变体名可读（不含 `Span` 噪声）；
//! ② `break` 跳到**循环出口块**、`continue` 跳到**循环条件块**（不是块 0）；
//! ③ 管道降成**值调用**（左右 operand 顺序正确）；
//! ④ 区域构造在产物里**看得见**（`Scope` + 正确的 kind）；
//! ⑤ 闭包的捕获变量**非空**（过近似）；
//! ⑥（D483）曾经「有损」的 6 类构造（`eval` / `aggregate` / quasiquote /
//!    `commit` / `rollback` / `orchestrate`）现在**忠实可见** —— 原语存在 +
//!    载荷逐字段对得上；
//! ⑦（D483）`eval` / `aggregate` 的**值寄存器不许被 Nil 覆盖**（D481/D482
//!    的占位路径会把语句的值从「given / 被贡献表达式的值」改成 Nil）；
//! ⑧（D485）区域**载荷**（`with_config` 的绑定 / `span`·`observe` 的名字与
//!    标签 / `export` 的名字）在产物里看得见 —— 缺了它们区域就「看得见但
//!    执行不了」。

use mora::mir::core::{CoreInst, CoreTerminator, ScopePayload};
use mora::value::Value;

/// 构造一个程序的 CoreFunction（走 9 层里的 EHIR → Core 段）。
fn core_of(src: &str) -> mora::mir::core::CoreFunction {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let fcfg = mora::mir::witness_to_fcfg::witness_to_fcfg(&witnesses);
    let table = mora::typeck::export::export_type_table(&witnesses);
    let ehir = mora::typeck::annotate::annotate(&fcfg, &table);
    mora::mir::ehir_to_core::ehir_to_core(&ehir, vec![], func.effects.clone())
}

/// 产物里所有 `Scope` 的 kind。
fn scope_kinds(core: &mora::mir::core::CoreFunction) -> Vec<String> {
    let mut out = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::Scope { kind, .. } = inst {
                out.push(kind.clone());
            }
        }
    }
    out
}

/// 判据 ①：`match` 分支保留**绑定名**，变体名**可读**（不含 Span 噪声）。
#[test]
fn match_arms_keep_bindings_and_readable_variants() {
    let core = core_of("let x = 3\nmatch x with\n  3 -> print(1)\n  y -> print(y)\nend\n");
    let mut arms = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::EnumMatch { arms: a, .. } = inst {
                arms.extend(a.iter());
            }
        }
    }
    assert!(!arms.is_empty(), "应产生 match 分支");
    // 变体名可读：模式存的是**源码文本**（不含 Debug 的 Span 噪声）
    for arm in &arms {
        assert!(
            !arm.pattern.contains("Span"),
            "变体名里混进了源码位置：{:?}",
            arm.pattern
        );
    }
    // 绑定名被保留（`y -> …` 的 `y`）
    let bound: Vec<&String> = arms.iter().flat_map(|a| a.bindings.iter()).collect();
    assert!(
        bound.iter().any(|b| b.as_str() == "y"),
        "模式里的绑定名 `y` 丢失了：{bound:?}"
    );
    // 变体名可读：变量模式的名字直接就是名字
    let labels: Vec<&String> = arms.iter().map(|a| &a.pattern).collect();
    assert!(
        labels
            .iter()
            .any(|l| l.as_str() == "y" || l.as_str() == "3"),
        "变体名不可读：{labels:?}"
    );
}

/// 判据 ②：`break` 跳到**循环出口块**、`continue` 跳到**循环条件块**。
///
/// D481 之前两者都是 `CoreInst::Jump(0)`（一条**假边**：目标是块 0，与循环
/// 出口毫无关系），而且只是块里的**一条指令** —— 块本身照常往下走。
///
/// 判据按**结构关系**写，不靠「块 0 是入口」这种位置巧合（循环的条件块
/// 完全可能就是块 0）：
///
/// ① `continue` 的目标必须是**带 Branch 终结指令**的块（条件块）；
/// ② `break` 的目标必须**不是**条件块（是出口块）；
/// ③ 必须存在一个 Branch 块 C：`C.id == continue 的目标` 且
///    `C.false_bb == break 的目标`（条件的假分支 = 循环出口 = break 的目标）；
/// ④ break / continue 的块里**不得再有** `CoreInst::Jump` 指令 ——
///    它们是**终结指令**，不是块里的普通指令。
#[test]
fn break_and_continue_target_the_right_blocks() {
    let core =
        core_of("let i = 0\nwhile i < 3\n  if i == 1 then\n    break\n  end\n  continue\nend\n");

    // 以「终结指令是 Jump、且块里没有 Jump 指令」为特征，找出实现
    // break / continue 的块（D481 之前它们会带一条 `CoreInst::Jump(0)`）。
    let jump_only: Vec<(usize, usize)> = core
        .blocks
        .iter()
        .filter(|b| {
            matches!(b.terminator, CoreTerminator::Jump(_))
                && !b.insts.iter().any(|i| matches!(i, CoreInst::Jump(_)))
        })
        .filter_map(|b| match b.terminator {
            CoreTerminator::Jump(t) => Some((b.id, t)),
            _ => None,
        })
        .collect();

    let is_branch_block = |id: usize| {
        core.blocks
            .get(id)
            .is_some_and(|b| matches!(b.terminator, CoreTerminator::Branch { .. }))
    };

    // ① continue 的目标是一个带 Branch 的块
    let continue_target = jump_only
        .iter()
        .find(|(_, t)| is_branch_block(*t))
        .map(|(_, t)| *t)
        .expect("应有一个跳回条件块的跳转（continue）");
    // ② break 的目标不是条件块
    let break_target = jump_only
        .iter()
        .find(|(_, t)| !is_branch_block(*t))
        .map(|(_, t)| *t)
        .expect("应有一个跳向循环出口的跳转（break）");
    assert_ne!(
        break_target, continue_target,
        "break 与 continue 不该跳到同一个块"
    );
    // ③ 条件的假分支 == 循环出口 == break 的目标
    let cond = core
        .blocks
        .iter()
        .find(|b| {
            b.id == continue_target
                && matches!(
                    &b.terminator,
                    CoreTerminator::Branch { false_bb, .. } if *false_bb == break_target
                )
        })
        .unwrap_or_else(|| {
            panic!("找不到「条件块」：id={continue_target} 且 false_bb={break_target} 的 Branch 块")
        });
    assert!(matches!(cond.terminator, CoreTerminator::Branch { .. }));
    // ④ break / continue 块里不得再有 Jump 指令（它们是终结指令）
    for (id, t) in &jump_only {
        assert!(
            !core.blocks[*id]
                .insts
                .iter()
                .any(|i| matches!(i, CoreInst::Jump(_))),
            "块 {id} 里有 `CoreInst::Jump` 指令（D481 之前的假边占位），\
             而它自己已经是 Jump 终结 ⇒ 目标 {t} 无意义"
        );
    }
}

/// 判据 ③：管道降成**值调用**，且左右 operand 顺序正确。
///
/// 管道语义 = 「用 right 调 left」（MIR 的 `h_pipe`：`call_value(&rhs, [lhs])`）。
/// D481 曾把它写成 `Call(dst, left, [right])` ⇒ **左右反了**（成了「用 left
/// 调 right」）—— 影子层不被执行，所以当时没暴露；Core 层要能执行前必须纠正。
#[test]
fn pipe_lowers_to_a_value_call_with_correct_operand_order() {
    let src = "let f = fn(x) x + 1\nlet y = 1 |> f\n";
    // 取 FCFG 里 Pipe 节点的 left / right 寄存器
    let (_func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let fcfg = mora::mir::witness_to_fcfg::witness_to_fcfg(&witnesses);
    let (left, right) = find_pipe(&fcfg).expect("FCFG 里应有 Pipe 节点");

    let core = core_of(src);
    let calls: Vec<(usize, usize, Vec<usize>)> = core
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter_map(|i| match i {
            CoreInst::ClosureCall(d, c, a) => Some((*d, *c, a.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !calls.is_empty(),
        "管道应降成**值调用** `ClosureCall`（D481 之前是 `Const(dst, Nil)`，两端全丢）"
    );
    assert!(
        calls
            .iter()
            .any(|(_, callee, args)| *callee == right && args == &vec![left]),
        "管道的 callee 必须是 **right**、实参是 **left**\
         （MIR 的 h_pipe: call_value(&rhs, [lhs])）；实际 {calls:?}\
         （FCFG: left={left} right={right}）"
    );
}

/// 在 FCFG 节点树里找 `Pipe` 节点，返回 (left, right) 寄存器。
fn find_pipe(nodes: &[mora::mir::fcfg::Node<()>]) -> Option<(usize, usize)> {
    use mora::mir::fcfg::Node;
    for n in nodes {
        match n {
            Node::Pipe { left, right, .. } => return Some((*left, *right)),
            Node::Sequence { nodes, .. } => {
                if let Some(x) = find_pipe(nodes) {
                    return Some(x);
                }
            }
            Node::Let { body, .. } => {
                if let Some(x) = find_pipe(&body.nodes) {
                    return Some(x);
                }
            }
            _ => {}
        }
    }
    None
}

/// 判据 ④：区域构造在产物里**看得见**。
#[test]
fn region_constructs_are_visible_as_scopes() {
    let cases: [(&str, &str, &str); 3] = [
        ("parallel", "parallel\n  print(1)\nend\n", "parallel"),
        (
            "observe",
            "observe trace \"t\" do\n  print(1)\nend\n",
            "observe",
        ),
        ("export", "export let a = 1\n", "export"),
    ];
    for (tag, src, want) in cases {
        let core = core_of(src);
        let kinds = scope_kinds(&core);
        assert!(
            kinds.iter().any(|k| k == want),
            "[{tag}] 区域构造应表现为 Scope(kind={want:?})，实际 {kinds:?}"
        );
    }
}

/// 判据 ⑤：闭包的捕获变量**非空**（过近似；D481 之前恒为空 + 注释说谎）。
#[test]
fn closure_captures_are_computed() {
    let core = core_of("let n = 1\nlet f = fn() n\n");
    let mut captures = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::ClosureCreate { captures: c, .. } = inst {
                captures.extend(c.iter().cloned());
            }
        }
    }
    assert!(
        captures.iter().any(|c| c == "n"),
        "闭包应捕获外层的 `n`（D481 之前 captures 恒为空），实际 {captures:?}"
    );
}

/// 判据 ⑧（D485）：区域**载荷**在 Core 产物里**看得见**。
///
/// D485 之前 `Scope` 只带 `kind` + 体 ⇒ `with_config` 的**绑定**、`span` /
/// `observe` 的**名字与标签**、`export` 的**名字**全丢 ⇒ 区域在 Core 层
/// 「**看得见但执行不了**」（执行器拿不到自己的数据）。
#[test]
fn region_payloads_are_visible() {
    // ── with_config：绑定名 + 值所在寄存器 ──
    let core = core_of("with model = \"m\"\n  1\nend\n");
    match scope_payload(&core, "with_config") {
        Some(ScopePayload::WithConfig(bindings)) => {
            assert_eq!(bindings.len(), 1, "应恰好一条配置绑定：{bindings:?}");
            assert_eq!(bindings[0].0, "model", "绑定名丢了：{bindings:?}");
        }
        other => panic!("with_config 应带绑定载荷，实际 {other:?}"),
    }

    // ── span：名字 + 标签 ──
    let core = core_of("span \"s\" tags {a: \"b\"} do\n  1\nend\n");
    match scope_payload(&core, "span") {
        Some(ScopePayload::Trace { name, tags }) => {
            assert_eq!(name, "s", "span 名丢了");
            assert!(
                tags.iter().any(|(k, v)| k == "a" && v == "b"),
                "span 标签丢了：{tags:?}"
            );
        }
        other => panic!("span 应带 trace 载荷，实际 {other:?}"),
    }

    // ── observe：名字带 `observe/` 前缀（与 MIR 侧 `h_observe` 的
    //    `format!("observe/{}", config)` 同款；config = kind + 名字）──
    let core = core_of("observe trace \"t\" do\n  1\nend\n");
    match scope_payload(&core, "observe") {
        Some(ScopePayload::Trace { name, .. }) => {
            assert_eq!(name, "observe/trace t", "observe 的 span 名不对：{name}");
        }
        other => panic!("observe 应带 trace 载荷，实际 {other:?}"),
    }

    // ── export：被导出的名字 ──
    let core = core_of("export let e = 1\n");
    match scope_payload(&core, "export") {
        Some(ScopePayload::Export(names)) => {
            assert!(names.iter().any(|n| n == "e"), "导出名丢了：{names:?}");
        }
        other => panic!("export 应带名字载荷，实际 {other:?}"),
    }
}

/// 产物里某个 kind 的区域**载荷**。
fn scope_payload<'a>(
    core: &'a mora::mir::core::CoreFunction,
    kind: &str,
) -> Option<&'a ScopePayload> {
    core.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .find_map(|i| match i {
            CoreInst::Scope {
                kind: k, payload, ..
            } if k == kind => Some(payload),
            _ => None,
        })
}

/// 判据 ⑥（D483）：曾经「有损」的 6 类构造，在 Core 产物里**忠实可见**。
///
/// D481/D482 时代它们只能写 `Const(dst, Nil)` 占位（编排更是用**空区域**
/// 冒充）⇒ 产物里**看不见**构造，更看不见载荷。
/// D483 起每个都有对应原语（形状**镜像** MIR 层）⇒ 本判据逐个核对
/// **原语存在** + **载荷逐字段对得上**。
#[test]
fn formerly_lossy_constructs_are_now_faithful() {
    // ── eval：断言名 / expects / 容差 / 回放路径都要在 ──
    let core = core_of("eval \"n\" 1, 2\n");
    let evals = collect(&core, |i| match i {
        CoreInst::Eval {
            name,
            expects,
            tolerance,
            replay_path,
            ..
        } => Some((
            name.clone(),
            expects.clone(),
            *tolerance,
            replay_path.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        evals.len(),
        1,
        "应恰好有一条断言原语（D481 之前是 `Const(dst, Nil)`，断言名与 expects 全丢），实际 {evals:?}"
    );
    let (name, expects, tolerance, replay_path) = &evals[0];
    assert_eq!(name, "n", "断言名丢了");
    assert_eq!(expects.len(), 1, "expects 丢了");
    assert_eq!(*tolerance, None, "容差丢了");
    assert_eq!(*replay_path, None, "回放路径丢了");

    // ── aggregate：聚合器名 + 被贡献的值都要在 ──
    let core = core_of("aggregate acc, 1\n");
    let aggs = collect(&core, |i| match i {
        CoreInst::Aggregate { name, value } => Some((name.clone(), *value)),
        _ => None,
    });
    assert_eq!(
        aggs.len(),
        1,
        "应恰好有一条聚合原语（D481 之前是 `Const(dst, Nil)`，聚合器名全丢），实际 {aggs:?}"
    );
    assert_eq!(aggs[0].0, "acc", "聚合器名丢了");

    // ── quasiquote：片段（静态文字 + 寄存器引用）都要在 ──
    let core = core_of("let x = 5\nlet q = `10+,x\n");
    let quotes = collect(&core, |i| match i {
        CoreInst::Quasiquote { segments, .. } => Some(segments.clone()),
        _ => None,
    });
    assert_eq!(
        quotes.len(),
        1,
        "应恰好有一条 quasiquote 原语（D481 之前是 `Const(dst, Nil)`，片段全丢），实际 {quotes:?}"
    );
    let segments = &quotes[0];
    assert_eq!(
        segments.len(),
        2,
        "`10+,x` 应是「静态片段 + 一个插值」两段，实际 {segments:?}"
    );
    assert!(
        matches!(&segments[0], mora::mir::QuasiquoteSegment::Quote(s) if s == "10+"),
        "第一段应是静态文字 `10+`，实际 {:?}",
        segments[0]
    );
    assert!(
        matches!(&segments[1], mora::mir::QuasiquoteSegment::Unquote(_)),
        "第二段应是寄存器插值（`,x`），实际 {:?}",
        segments[1]
    );

    // ── commit / rollback：这两件事在产物里必须**看得见** ──
    // （此前只有 `Const(dst, Nil)` ⇒ 与「别的语句返回 Nil」完全无法区分。）
    let core = core_of("transaction\n  print(1)\nend\ncommit\n");
    assert_eq!(
        count(&core, |i| matches!(i, CoreInst::Commit)),
        1,
        "提交在产物里应看得见（D481 之前只有 Nil 占位）"
    );
    let core = core_of("rollback\n");
    assert_eq!(
        count(&core, |i| matches!(i, CoreInst::Rollback)),
        1,
        "回滚在产物里应看得见（D481 之前只有 Nil 占位）"
    );

    // ── orchestrate：载荷（agent 图 / 边）要**原样**在 ──
    let core = core_of(
        "orchestrate graph input -> result\n  agent a => \"A\"\n  agent b => \"B\"\n  @start -> a\n  a -> b\nend\n",
    );
    let orchs = collect(&core, |i| match i {
        CoreInst::Orchestrate {
            input_var,
            result_var,
            kind,
        } => Some((input_var.clone(), result_var.clone(), kind.clone())),
        _ => None,
    });
    assert_eq!(
        orchs.len(),
        1,
        "应恰好有一条编排原语（D481 之前是**空区域** Scope{{kind:\"orchestrate\"}}，载荷全丢），实际 {orchs:?}"
    );
    let (input_var, result_var, kind) = &orchs[0];
    assert_eq!(input_var, "input", "编排的输入变量名丢了");
    assert_eq!(result_var, "result", "编排的结果变量名丢了");
    match kind {
        mora::mir::witness::WitnessOrchestrateKind::Graph { agents, edges } => {
            assert_eq!(agents.len(), 2, "编排的 agent 列表丢了：{agents:?}");
            assert_eq!(edges.len(), 2, "编排的边列表丢了：{edges:?}");
        }
        other => panic!("编排载荷的种类不对：{other:?}"),
    }
}

/// 判据 ⑦（D483）：`eval` / `aggregate` 的**值寄存器不许被 Nil 覆盖**。
///
/// D481/D482 时代这两个构造降成 `Const(dst, Nil)`，而 `dst` 与 `given` /
/// `value` **是同一个寄存器** ⇒ 那条 Nil 把值**覆盖**掉了 ⇒ 语句的值变成
/// Nil，与 MIR 层（值 = given / 被贡献表达式的值）**分叉**。
///
/// 本判据按**位置关系**写：某个 `Eval` / `Aggregate` 之后，**不得**再出现
/// 往同一个寄存器写 Nil 的指令。
#[test]
fn eval_and_aggregate_values_are_not_clobbered_by_nil() {
    let cases: [(&str, &str); 2] = [
        ("eval", "eval \"n\" 1, 2\n"),
        ("aggregate", "aggregate acc, 1\n"),
    ];
    for (tag, src) in cases {
        let core = core_of(src);
        let mut seen = 0usize;
        for b in &core.blocks {
            for (i, inst) in b.insts.iter().enumerate() {
                // 这两个构造的值寄存器
                let value_reg = match inst {
                    CoreInst::Eval { given, .. } => *given,
                    CoreInst::Aggregate { value, .. } => *value,
                    _ => continue,
                };
                seen += 1;
                for later in &b.insts[i + 1..] {
                    if let CoreInst::Const(r, Value::Nil) = later
                        && *r == value_reg
                    {
                        panic!(
                            "[{tag}] 块 {} 里，值寄存器 {value_reg} 在构造之后被 `Const(_, Nil)` \
                             覆盖了（D481/D482 的占位路径）⇒ 语句的值不再是 \
                             given / 被贡献表达式的值",
                            b.id
                        );
                    }
                }
            }
        }
        // ⚠ 防「空转通过」：若原语整个消失（降维退回占位），本判据会因为
        // 「找不到可检查的构造」而**恒真** ⇒ 必须先确认确实找到了。
        assert!(
            seen >= 1,
            "[{tag}] 产物里应至少有一条对应的原语，却一条都没有 \
             （降维退回占位路径了？）"
        );
    }
}

/// 产物里所有满足条件的指令（顶层块）。
fn collect<T>(core: &mora::mir::core::CoreFunction, f: impl Fn(&CoreInst) -> Option<T>) -> Vec<T> {
    core.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter_map(f)
        .collect()
}

/// 产物里满足条件的指令**条数**。
fn count(core: &mora::mir::core::CoreFunction, f: impl Fn(&CoreInst) -> bool) -> usize {
    core.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter(|i| f(i))
        .count()
}

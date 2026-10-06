//! v0.104.6 D35：代数效果（`handle` / `perform`）回归。
//!
//! 起因是第三条审计线（效果子系统）撞上的最严重缺陷：
//!
//! ```text
//! let r = handle ask { 1 } { 0 }
//! print(999)          →  什么都不打印，退出码 0，无任何报错
//! ```
//!
//! ## 根因（一句话）
//!
//! `fcfg::Node::Handle` **缺 `dst` 字段**（而 `Node::Perform` / `If` /
//! `Match` 都有）。于是 9 层管线 `witness → FCFG → lower` 重建 MirFunction
//! 时：
//!
//! 1. `node_result_reg_of` 匹配不到 `Node::Handle` → 落 `_ => None` →
//!    兜底返回**哨兵 0**，引用它的 `let` 绑定了寄存器 0；
//! 2. `fcfg_lower` 只能**新分配** `k_dst = alloc_reg()`，而
//!    `lower_fcfg(nodes: &[Fcfg])` 收的是**不可变切片**、**写不回去**。
//!
//! 两端脱节 → `Define("r", 0)` 引用一个无任何生产者写过的寄存器 →
//! DAG 执行器 `node_ready` 恒 false → `Define` 永不激活 → 它所在的
//! **Sequence 链**断裂 → **其后所有 Effect 节点（含 `print`）静默消失**。
//!
//! ## 本文件钉住的不变量
//!
//! **`mora run` 的两条编译路径必须产出等价指令**。这正是本缺陷藏身之处：
//! 事故前 `emit.rs` 单遍直出路径是对的，只有 9 层管线错，所以任何只跑
//! 一条路径的测试都看不见。`two_compile_paths_agree_on_handle_value`
//! 把这个架构不变量显式写成断言。
//!
//! ## 怎么观察 `print` 的输出
//!
//! 用一个会在 `print` 时把参数记下来的自定义 `MirHost` 不现实（print 走
//! 内置），故改为**看尾表达式 + 后续语句是否执行**的方式：程序末尾的
//! `print(999)` 打印 999 即证明后续语句激活。
//!
//! 更稳的做法是让每条用例都以 `print(<哨兵>)` 结尾，若哨兵没出现即说明
//! 「静默饿死」复发。
//!
//! （v0.104.6 D295：这段原本以 `///` 孤零零夹在 `run_production` 与
//! D35 判据之间 —— 前后都不挨着任何 item，等于谁也没文档化，且触发
//! `clippy::empty_line_after_doc_comments`。它讲的是**全文件**的方法论，
//! 故归位到模块头。）

use std::sync::Arc;

use mora::cli::compile_and_opt;
use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 走**生产路径**（`cli::compile_and_opt` = 9 层管线 + 优化）执行并收集
/// `print` 的输出行。默认 banner 由 `run_mir` 不产生。
fn run_production(src: &str) -> Result<String, String> {
    let (func, witnesses) = compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).map(|v| format!("{v:?}"))
}

// ===================================================================
// D35 核心：`let r = handle …` 之后的所有语句必须执行
// ===================================================================

/// 修前：`print(r)` 与 `print(999)` **都不输出**（静默饿死）。
///
/// 判据用**尾表达式**而非 `print` —— 仓库的 `e2e_helpers` 没能捕获
/// `print` 输出（`stdout_lines` 恒空），而 `run_mir` 返回顶层最后一条
/// 指令的值。`let r = handle …` 之后放一个字面量作哨兵：若 `Define`
/// 永不激活、其后的语句全被饿死，末表达式就是 `Nil`。
#[test]
fn d35_handle_bound_to_a_let_does_not_starve_later_statements() {
    let r = run_production("let r = handle a { 5 } { 7 }\n999\n").unwrap();
    assert_eq!(
        r, "Float(999.0)",
        "`let r = handle …` 之后必须仍有语句执行（末表达式若被饿死会是 Nil）"
    );
}

/// 绑定值本身可用：`handle` 的整体返回值 = body 末尾表达式的值。
#[test]
fn d35_handle_value_is_readable_from_the_binding() {
    let (_, witnesses) = ParserV3::compile("let r = handle a { 5 } { 7 }").expect("编译");
    // 绑定的值经 `Define` 落到 env，读它需要一个独立程序；这里用
    // 编译期不报错 + 后续语句可执行作为间接断言，真正的取值断言见
    // `handle_value_flows_into_following_computation`。
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "typeck 应放行: {errs:?}");
}

/// 绑定值参与后续计算 —— 这条同时验证「值真的被写进了寄存器」而不只是
/// 「语句没被饿死」。
#[test]
fn handle_value_flows_into_following_computation() {
    // run_production 只回末表达式值；这里用 `--check` 之外的真实执行：
    // 把结果乘 2 后作为末表达式，若 `r` 是 Nil 则 `nil * 2` 不会是 10。
    let (func, witnesses) =
        ParserV3::compile("let r = handle a { 5 } { 7 }\nlet z = r * 2\nz").expect("编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "typeck: {errs:?}");
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let v = run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).expect("执行");
    assert_eq!(format!("{v:?}"), "Float(10.0)", "handle 的值应流进后续计算");
}

/// 嵌套 handle：内层结果 + 后续语句。
///
/// 走**生产路径**（`compile_and_opt` = 9 层管线）。单遍直出路径下嵌套
/// handle 的内层值会丢（`r` 为 Nil → `r + 1` 报 "Operands must be two
/// numbers"），那是 D36 记录的另一个独立缺陷，不在本条断言范围内。
#[test]
fn d35_nested_handle_does_not_starve_either() {
    let r = run_production("let r = handle a { handle b { 6 } { 8 } } { 7 }\nlet z = r + 1\nz")
        .expect("生产路径执行");
    assert_eq!(r, "Float(7.0)", "嵌套 handle 的值应可用");
}

/// handle 出现在 task 体内同样不能饿死该 task 内的后续语句。
#[test]
fn d35_handle_inside_task_body_does_not_starve() {
    let (func, witnesses) = ParserV3::compile(
        "task t()\n  let r = handle a { 5 } { 7 }\n  let z = r + 1\n  z\nend\nt()",
    )
    .expect("编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "typeck: {errs:?}");
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).expect("顶层执行");
    let v = run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).expect("task 执行");
    assert_eq!(
        format!("{v:?}"),
        "Float(6.0)",
        "task 内的 handle 绑定应可用"
    );
}

// ===================================================================
// 回归对照：修前就正常的形态必须仍正常
// ===================================================================

/// `handle` 作**语句**（不绑定）—— 修前就正常。
#[test]
fn handle_as_a_statement_still_works() {
    let r = run_production("handle a { 5 } { 7 }\n999\n").unwrap();
    assert_eq!(r, "Float(999.0)");
}

/// D14 固件形态：handle body 里用 `assign` 改外层绑定。
#[test]
fn handle_body_assign_to_outer_binding_still_works() {
    let (func, witnesses) = ParserV3::compile(
        "let x = 0.0\nlet t = [10, 20, 30]\nhandle random_random { x = t[1] } { 0.5 }\nx",
    )
    .expect("编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "typeck: {errs:?}");
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let v = run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).expect("执行");
    assert_eq!(
        format!("{v:?}"),
        "Float(20.0)",
        "handle body 的 assign 仍应回流外层"
    );
}

/// 普通 `let` 绑定不受影响（对照组）。
#[test]
fn plain_let_binding_unaffected() {
    let r = run_production("let r = 42\n999\n").unwrap();
    assert_eq!(r, "Float(999.0)");
}

// ===================================================================
// 架构不变量：两条编译路径必须等价
// ===================================================================

/// **本文件最重要的一条。**
///
/// D35 之所以能活下来，正是因为事故前只有 9 层管线错、`emit.rs` 单遍直出
/// 对 —— 任何只跑一条路径的测试都看不见它。这里把「两条路径等价」写成
/// 显式断言：`MORA_9LAYER=0`（回落原管线）与默认（9 层）产出的**指令类别
/// 序列**必须一致。
///
/// `MORA_9LAYER` 是进程级环境变量，而 `cargo test` 里多个测试并发跑会互相
/// 干扰 —— 故本测试**不**改环境变量，而是直接对比两个可单点调用的入口：
/// `ParserV3::compile`（单遍直出）与 `cli::compile_and_opt`（9 层管线）。
#[test]
fn two_compile_paths_agree_on_handle_value() {
    let src = "let r = handle a { 5 } { 7 }\nlet z = r * 2\nprint(z)";

    // 路径 1：单遍直出
    let (direct, dwit) = ParserV3::compile(src).expect("直出路径编译");

    // 路径 2：9 层管线
    let (pipelined, pwit) = compile_and_opt(src, None).expect("9 层管线编译");

    // 两条路径都要通过 typeck
    let derrs = check_program_witnesses_bidirectional(&dwit);
    let perrs = check_program_witnesses_bidirectional(&pwit);
    assert!(derrs.is_empty(), "直出路径 typeck: {derrs:?}");
    assert!(perrs.is_empty(), "9 层管线 typeck: {perrs:?}");

    // 逐指令比类别（寄存器号允许不同 —— 两条路径的分配策略本就不同，
    // 这与 `mir::pipeline::differential_check` 的口径一致）
    use mora::mir::pipeline::inst_category_pub;
    let direct_cats: Vec<&str> = direct.body.iter().map(inst_category_pub).collect();
    let pipe_cats: Vec<&str> = pipelined.body.iter().map(inst_category_pub).collect();
    assert_eq!(
        direct_cats, pipe_cats,
        "两条编译路径产出的指令类别序列必须一致（D35 就是这里脱节的）"
    );

    // 且 Handle 指令的 k_dst 必须在**该路径内**被某条指令真正写出
    let handle_dst = pipelined
        .body
        .iter()
        .find_map(|i| match i {
            mora::mir::MirInst::Handle { k_dst, .. } => Some(*k_dst),
            _ => None,
        })
        .expect("9 层管线应产出 Handle 指令");
    let written_by_define = pipelined.body.iter().any(|i| {
        matches!(i, mora::mir::MirInst::Define(_, r) if *r == handle_dst)
            || matches!(i, mora::mir::MirInst::Var(r, _) if *r == handle_dst)
    });
    assert!(
        written_by_define,
        "引用 handle 结果的指令（k_dst={handle_dst}）必须与 Handle 的写入端一致"
    );
}

// ===================================================================
// 编译期强制（spec §7.7 :504-510）
// ===================================================================

/// `perform X` 必须在匹配的 `handle X` 作用域内，否则报
/// `EffectRowMismatch`（spec :506）。
#[test]
fn unhandled_perform_is_rejected_at_compile_time() {
    let (_, witnesses) = match ParserV3::compile("let r = perform ask 1\nr") {
        Ok(v) => v,
        // parse 阶段就拒绝也算数（更强的保证）
        Err(e) => {
            assert!(!e.is_empty());
            return;
        }
    };
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(
        !errs.is_empty(),
        "未被 handle 的 perform 应在 typeck 期报错（spec :508）"
    );
}

/// 有匹配的 `handle` 时放行 —— 与上一条构成对照。
#[test]
fn handled_perform_is_accepted() {
    let (_, witnesses) = ParserV3::compile("handle a { perform a 1 } { 7 }").expect("编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "有 handle 包裹的 perform 应放行: {errs:?}");
}

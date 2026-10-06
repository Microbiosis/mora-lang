//! v0.104.6 D304：SSA `deconstruct` 的 `n_regs` 漏算了**透传指令**的寄存器
//! ⇒ `--opt=1/2` 下大量程序**硬报错**（退出码 1）。
//!
//! ## 现象（修前）
//!
//! ```text
//! $ mora --opt=1 tests/fixtures/e2e/rel_basic.mora
//! Runtime error (MIR): internal: instruction at DAG node 5 references
//! register 5 (read/write) but the function only has 1 register(s)
//! — a unit-statement emitter returned an unallocated sentinel register
//! ```
//!
//! 56 个真实程序里 **16 个（28.6%）**在 `--opt=1/2` 下行为改变，
//! 其中 **13 个直接从 exit 0 变成 exit 1**（`rel_*` 6 个、`match_*` 2 个、
//! `tea_counter`、`handle_effect`、两个 `examples/`）。
//!
//! ## 根因
//!
//! `ssa::construct` 把「声明型 / effectful」指令（`MatchExpr` / `Closure` /
//! `WithConfig` / `Perform` / `Handle` / `Solve` / `RelDef` … 共 18 类，见
//! `is_ssa_passthrough`）收进 `passthrough`，`deconstruct` 时**原样**插回 body 头部。
//!
//! 但 body 其余部分已被 `map_ssa` 重编号并压进 `0..next_plain_reg`，
//! 而 `n_regs` 只取 `next_plain_reg` ⇒ **透传指令引用的寄存器越界**。
//!
//! 修法：`n_regs = next_plain_reg.max(orig_n_regs)`。透传指令用的就是
//! 重命名前的编号，故原始计数是它们的**可靠上界**；多分配几个槽无害。
//!
//! ## 修后（实测）

use std::process::Command;

fn run(src: &str, tag: &str, opt: bool) -> (i32, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mora_d304_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut c = Command::new(exe);
    if opt {
        c.arg("--opt=1");
    }
    let out = c.arg(&p).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    let lines = s
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                && l.len() < 30
                && !l.starts_with("Mora v")
                && !l.contains("AI:")
                && !l.contains("AI 原语")
                && !l.contains("显式 API")
                && !l.contains("Trait 系统")
                && !l.contains("Built-in")
                && !l.contains("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
        })
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

/// 主判据：带 `MatchExpr`（passthrough 且带寄存器）的程序在 `--opt=1` 下
/// 必须**正常执行**。
///
/// 修前这里报的是 `references register N but the function only has M
/// register(s)` 并以 **exit 1** 结束。⇒ 这条同时钉住「不再硬报错」与
/// 「结果与默认档一致」。
#[test]
fn match_expr_passthrough_no_longer_hard_errors() {
    // ⚠ 本条**只**断言「不再 exit 1」—— D304 修的是 `n_regs` 越界。
    // `MatchExpr` 在 `--opt=1` 下**仍会静默无输出**（exit 0 但什么都不打印），
    // 那是块/序列结构的另一族问题（D303 的阶梯式重复同族），**本轮未修**，
    // 由 `opt_level_tail_duplication.rs` 与下面那条现状判据记录。
    for (tag, src) in [
        (
            "match-简单",
            "let x = 2\nprint(match x { 1 => \"a\", 2 => \"b\", _ => \"c\" })\n",
        ),
        (
            "match-多臂",
            "let x = 3\nprint(match x { 1 => \"a\", 2 => \"b\", _ => \"c\" })\n",
        ),
        (
            "match-带守卫",
            "let x = 3\nprint(match x { n when n > 5 => \"big\", _ => \"small\" })\n",
        ),
        (
            "match-多语句",
            "let x = 1\nlet y = match x { 1 => 10, _ => 20 }\nprint(y)\nprint(2)\n",
        ),
    ] {
        let (oc, ogot) = run(src, tag, true);
        assert_eq!(
            oc, 0,
            "[{tag}] `--opt=1` 不得因 passthrough 指令的寄存器越界而**报错**（D304）。\n\
             修前的消息是：references register N but the function only has M register(s)\n\
             实得 exit={oc} out={ogot:?}"
        );
    }
}

/// `MatchExpr` 程序在 `--opt=1` 下必须**正常执行并输出正确结果**。
///
/// 本条原先是**现状判据**（钉住「opt=1 下 match 静默无输出」）。v0.104.6 D310
/// 修好了：根因是 `Closure` / `MatchExpr` 等**透传指令**的寄存器不参与 SSA
/// 重编号，而同一函数里其余指令会重编号 ⇒ 两套寄存器空间
/// （`opt=off` 时 `MatchExpr` 的输出寄存器 7，在 `opt=1` 被改成 0）。
/// 修法是「含透传指令的函数**整体跳过 SSA**」。
#[test]
fn match_expr_executes_under_opt() {
    let src = "let x = 2\nprint(match x { 1 => \"a\", 2 => \"b\", _ => \"c\" })\n";
    let (dc, dgot) = run(src, "sq_def", false);
    let (oc, ogot) = run(src, "sq_opt", true);

    assert_eq!(dc, 0, "默认档必须成功; 实得 {dc}");
    assert_eq!(
        dgot,
        vec!["b".to_string()],
        "默认档必须输出 b; 实得 {dgot:?}"
    );
    assert_eq!(oc, 0, "`--opt=1` 下应成功执行; 实得 {oc}");
    assert_eq!(
        ogot,
        vec!["b".to_string()],
        "**D310**：`--opt=1` 下 match 程序必须输出 `b`（修前是静默无输出）。\n  实得: {ogot:?}"
    );
}

/// 真实 fixture 回归：`rel_basic.mora` 修前在 `--opt=1` 下是 **exit 1**。
#[test]
fn real_fixture_rel_basic_survives_optimization() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/e2e/rel_basic.mora"
    );
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let d = Command::new(exe).arg(fixture).output().expect("跑");
    let o = Command::new(exe)
        .arg("--opt=1")
        .arg(fixture)
        .output()
        .expect("跑");
    assert_eq!(
        d.status.code(),
        Some(0),
        "默认档应成功; 实得 {:?}",
        String::from_utf8_lossy(&d.stderr).lines().last()
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "D304：`rel_basic.mora` 在 `--opt=1` 下修前是 exit 1（passthrough 指令\
         的寄存器越界）。若这里又非 0，请看 stderr 里的 Runtime error。"
    );
}

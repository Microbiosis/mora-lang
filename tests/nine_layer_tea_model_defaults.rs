//! v0.104.6 D412：9 层管线**对 TEA 程序永久回落** —— `model` 字段默认值
//! 在 witness 层就被丢弃（修复轮）
//!
//! ## 实测（修前）
//!
//! `tests/fixtures/e2e/tea_standalone.mora`（57 个夹具里**唯一**触发回落的一个）：
//!
//! ```text
//! [9layer] 差分失败：已回落到 emit.rs 路径 | pipeline_mir=5 original_mir=11
//! ```
//!
//! 差分明细：管线只发 5 条（`ModelDef, MsgDef, UpdateDef, AppDef, TaskDef`），
//! emit.rs 发 11 条。缺的正是 model 字段默认值那一段。
//!
//! ## 根因：信息在**源头**就没了
//!
//! `parser_v3/emit_definitions.rs::emit_model_def_w` 解析 `name: T = expr` 时：
//!
//! ```text
//! let (dreg, _dw) = self.emit_expr_w()?;      // dreg → emit 流；_dw → 丢弃
//! defaults.push((fname.clone(), dreg));
//! ```
//!
//! `emit_expr_w` **同时**返回寄存器与 witness，`_dw` 被下划线丢掉了
//! ⇒ `WitnessKind::ModelDef` 里没有 `defaults` ⇒ witness → Node → fcfg_lower
//! 整条 9 层链路**结构上拿不到默认值**，只能回落。
//!
//! 另有一处同族缺口：`emit.rs` 无条件发一条尾部 `Const(Nil)`，
//! `fcfg_lower` 此前也没有（与本文件同族的 `emit_loop_result` 注释记过
//! 「少发一条 Const ⇒ 差分失败」的历史教训）。
//!
//! ## 修法
//!
//! | 位置 | 改动 |
//! |---|---|
//! | `mir/witness.rs` | `WitnessKind::ModelDef` 增 `defaults: Vec<(String, MirWitness)>` |
//! | `parser_v3/emit_definitions.rs` | `_dw` 接上，填进 witness |
//! | `mir/fcfg.rs` | `Node::ModelDef` 增 `defaults: Vec<(String, Reg, Node<M>)>` |
//! | `mir/witness_to_fcfg.rs` | 转换；**寄存器在此一次算好**（`node_result_reg`） |
//! | `mir/fcfg_lower.rs` | 按 emit.rs 顺序发射 + `max_reg_in_node` 计入寄存器 |
//! | `typeck/annotate.rs` | 默认值表达式递归 annotate（`Node<()>` → `Node<TypeInfo>`） |
//!
//! ⚠ 顺序必须**逐条对齐** `emit_model_def_w`：
//! ① 默认值表达式 → ② `ModelDef` → ③ `DictLit`+`Define("Name.defaults")` → ④ 尾部 `Const(Nil)`。
//!
//! ## 修后
//!
//! 全语料回落率 **57/57 夹具 + 6/6 examples 全部 0 回落**（修前 1/57），
//! 且 `tea_standalone.mora` 的运行结果**一字未变**。
//!
//! ## 本文件为何单独成文件
//!
//! TEA 程序有**自己的一整套声明形态**（model / msg / update / app），
//! 与普通程序不共用降级路径，单独一个文件便于定位回归。
//!
//! ## v0.104.6 D474：判据从「两条编译路径一致」改为「期望输出」
//!
//! 原判据的 ①②（无 `[9layer]` 回落行 / `MORA_9LAYER=0` 的 A/B）**已作废**：
//! D474 把差分机制整个退役 —— parser 只产 witness、MIR 全部由 9 层产出 ⇒
//! 「参考实现」这一侧**已不存在**，`MORA_9LAYER=0` 会退化成自己跟自己比。
//!
//! ⇒ 保留 ③④：**运行结果必须逐行钉死**（不依赖任何对照物）。这比原判据
//! **更强** —— 原判据只能发现「两条路径不一致」（两条一起错到同一个错值
//! 时反而绿）。

use std::process::Command;

const TEA: &str = "tests/fixtures/e2e/tea_standalone.mora";

fn run_tea() -> (String, String, i32) {
    let exe = env!("CARGO_BIN_EXE_mora");
    let out = Command::new(exe)
        .arg("run")
        .arg(TEA)
        .output()
        .expect("应能执行 mora");
    (
        String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

/// **D412 主断言**：TEA 程序的运行结果必须是**钉死的 4 行**。
#[test]
fn d412_tea_model_defaults_produce_the_expected_output() {
    let (out, err, code) = run_tea();
    assert_eq!(code, 0, "TEA 程序应成功运行; stderr:\n{err}");

    // ① 运行结果**逐字**等于期望（防「两条路径一起错到同一个错值」）。
    assert_eq!(
        out, "dict\nlist\nclosure\ntea_app",
        "TEA 程序的 4 行输出应不变（type_of(Counter/CounterMsg/update/a)）; 实得 {out}"
    );

    // ② 逐个值都断言（防上面那个整体相等因换行/空白巧合而通过）。
    for want in ["dict", "list", "closure", "tea_app"] {
        assert!(
            out.lines().any(|l| l == want),
            "输出应含独立一行 `{want}`; 实得:\n{out}"
        );
    }
}

/// **源码级、自维护的护栏**：默认值必须**留在 witness 里**。
///
/// 行为判据（上面那条）能覆盖「差分是否通过」，但如果将来有人为了简化
/// 又把 `defaults` 从 witness 里删掉，本条会先一步变红并指出**在哪**。
///
/// ⚠ 判断「代码里没有 X」前**先剥注释行** —— D388 踩过：按字符串切片取
/// 函数体会把下一个 item 的 doc comment 一并带上。
#[test]
fn d412_witness_keeps_model_defaults() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mir/witness.rs"))
        .expect("读 witness.rs");
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
        .collect::<Vec<_>>()
        .join("\n");

    let at = code
        .find("ModelDef {")
        .expect("应能找到 WitnessKind::ModelDef");
    let body = &code[at..at + 600];
    assert!(
        body.contains("defaults"),
        "`WitnessKind::ModelDef` 里没有 `defaults` 字段 —— 9 层管线将再次拿不到 \
         model 字段默认值而永久回落（D412 已修）。实得:\n{body}"
    );
}

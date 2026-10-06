//! v0.104.6 D275/D276：9 层 IR 管线的差分在 21% 的真实程序上失败 ⇒ 生产路径永不验证它
//!
//! ## ⚠ 本文件是**现状判据**：它断言的是**当前**的失败集合
//!
//! 它今天通过，**恰恰是因为现状如此**。失败集合变化时它会**变红** ——
//! 那时请先读下面「D276 撤销」一节，再决定是改进还是回归。
//!
//! ## 现状：53 个真实 `.mora` 中 11 个差分失败（21%）
//!
//! 每次 `mora run` 都在 stderr 打这一行（`cli/mod.rs:71`，**无条件**打印）：
//!
//! ```text
//! [9layer] 差分失败：已回落到 emit.rs 路径（9 层管线产出被丢弃）| pipeline_mir=2 original_mir=3
//! ```
//!
//! ⇒ 这 21% 的程序上 **9 层产出被整段丢弃**，用户拿到 `emit.rs` 的结果。
//! 程序能跑（退出码与 stdout 不变），但项目自述的架构方向
//! **在生产中从未生效** —— 它在这些程序上的正确性**从未被验证**。
//!
//! ## D276：曾把这 11 个修成 1 个，**已撤销**
//!
//! D276 的诊断是**对**的：`nested_diffs` 用**原始**下标，而顶层比较用
//! `significant_categories`（已剔除死 `Const(r, Nil)`）—— 两套基准不一致，
//! 死 `Const` 让后续下标整体错位、拿无关指令互比。
//!
//! 按该诊断对齐后，失败数 11 → 1，**看起来是纯改进**。
//!
//! ### 但它引入了真回归，被既有测试抓到
//!
//! `tests/path_differential_census.rs::d122_every_e2e_fixture_gives_identical_results_on_both_compile_paths`
//! 报：
//!
//! ```text
//! pipelined = Err(internal: instruction at DAG node 5 references register 4
//!               (read/write) but the function only has 1 register(s) —
//!               a unit-statement emitter returned an unallocated sentinel register)
//! ```
//!
//! 真实 CLI 同样可复现：`rel_basic.mora` 默认路径 **exit 1**，
//! `MORA_9LAYER=0` 路径 **exit 0**。
//!
//! ⇒ **9 层的 `rel` 路径本身是坏的**（寄存器破损）。对齐之前，
//! `nested_diffs` 的「错位」**偶然地**通过条数差异把它挡在了生产之外。
//!
//! **根子**：差分**按设计不比较寄存器号**（D36 已记「寄存器级审计
//! opt-in、只诊断、不判失败」）。类别序列可以对齐，**寄存器破损看不见**。
//!
//! ⇒ 在 9 层 `rel` 路径修好之前，**必须保留**错位。本文件因此仍是
//! 「11 个必须回落」的现状判据。
//!
//! ## 我的 A/B 为什么一开始没抓到（方法教训）
//!
//! D276 期间的 A/B 对照 53 个程序报「52 同 1 异」，看起来安全。
//! **实际上我只比了 stdout** —— `rel_*` 程序两条路径的 stdout **都是空**
//! （它不打印任何东西），于是「相等」，
//! 而**退出码 1 vs 0 被漏掉了**。
//!
//! ⇒ **A/B 必须同时比 stdout 与退出码**。判「行为等价」时只比一个维度，
//! 等于没比 —— 这与「判据只断言一个维度」是同一条纪律。
//!
//! ## 一条判据自身的坑
//!
//! 首版用 `"differential FAILED"` 做判据，而那行**只在**
//! `MORA_9LAYER_DEBUG=1` 时打印（`cli/mod.rs:76`）；无条件打印的是
//! 「差分失败」摘要行 ⇒ 首版把 11 个失败**全报成 0**。已改用无条件那行。

use std::process::Command;

/// 跑一次 `mora run`，返回 stderr 里**无条件**打印的 9 层摘要行。
fn nine_layer_fell_back(path: &str) -> bool {
    let exe = env!("CARGO_BIN_EXE_mora");
    let out = Command::new(exe)
        .arg("run")
        .arg(path)
        .output()
        .expect("应能执行 mora");
    String::from_utf8_lossy(&out.stderr).contains("差分失败")
}

fn fixture(name: &str) -> String {
    format!("tests/fixtures/e2e/{name}")
}

/// **现状判据**：只剩 `tea_standalone.mora` **必须**回落。
///
/// **v0.104.6 D315 更新**：原断言是「这 11 个文件必须全部回落」。D314 修好
/// 9 层 `rel` 路径的 `n_regs` 破损（`max_reg_in_node` 的 `_ => 0` 漏算
/// `Solve` / `Return` / `WithConfig`）后，D315 重新应用了 D276 的差分对齐，
/// 其余 10 个**不再回落**。
///
/// 这 10 个的行为验证（**缺一不可**）：
/// - `rel` 强证人（**打印 `solve` 结果** —— fixture 自身无 `print`，
///   可观察行为只有「exit 0、无输出」，比不出对错）× 3 档 ⇒ 逐行相同；
/// - 5 种块形态 × 3 档 = 30 组合 ⇒ 逐行相同；
/// - 3 种声明形态（`msg` / `struct` / `enum`）× 3 档 = 30 组合 ⇒ 逐行相同；
/// - 56 个真实 `.mora` × 3 档 = 168 组合 ⇒ 逐行相同。
///
/// 普查里翻转的 8 条**全部同向**（回落 → 通过），无一条反向退化。
/// 常驻护栏见 `tests/nine_layer_unblocked.rs`。
///
/// `tea_standalone.mora` 仍回落的根因是 D95 记档的那一族
/// （`Parallel` / `Observe` / `Span` / `PromptSection` / `DocumentSection`
/// 五个分支不补 `Const(dst, Nil)`），补齐需给 `Node` 加 `dst` 字段并改
/// `witness_to_fcfg` 与 `node_result_reg` —— 核心 AST 的结构性改动，未做。
///
/// 失败集合变化时本条会红 —— **先读文件头再判断是改进还是回归**
/// （D276 就是一次「看起来是改进、实则是回归」；D315 之所以敢翻，是因为
/// D314 先把 D276 当年撞上的那个 `n_regs` 崩溃修好了）。
#[test]
fn d275_only_tea_standalone_still_falls_back() {
    let must_fall_back = ["tea_standalone.mora"];
    let now_on_pipeline = [
        "rel_basic.mora",
        "rel_cons.mora",
        "rel_empty.mora",
        "rel_project.mora",
        "rel_run_limit.mora",
        "rel_single_var.mora",
        "rel_zero_var.mora",
        "export_visibility.mora",
        "import_handle_index_main.mora",
        "prompt_section.mora",
    ];

    for name in must_fall_back {
        let path = fixture(name);
        assert!(
            std::path::Path::new(&path).exists(),
            "fixture 不存在：{path}"
        );
        assert!(
            nine_layer_fell_back(&path),
            "`{name}` 当前**必须**回落（差分失败；D95 记档的 `Const(dst, Nil)` 缺口）。\n\
             若它不再回落，说明那 5 个分支已补齐 —— 那时本判据与 \
             `nine_layer_fallback_census.rs` 的记档都应同步更新。"
        );
    }

    // 反向断言：原本回落的 10 个必须**确实**走上管线了（双向牙齿）。
    let mut still_falling_back = Vec::new();
    for name in now_on_pipeline {
        let path = fixture(name);
        assert!(
            std::path::Path::new(&path).exists(),
            "fixture 不存在：{path}"
        );
        if nine_layer_fell_back(&path) {
            still_falling_back.push(name.to_string());
        }
    }
    assert!(
        still_falling_back.is_empty(),
        "**D315**：这 10 个文件应当**已走上 9 层管线**。仍回落的有：\n\
         {still_falling_back:?}\n\
         若差分对齐被撤销，请一并复查 D314 的 `max_reg_in_node` 穷尽 match\
         是否还在（那正是 D276 撤销的原因）。"
    );
}

/// 对照组：本来**就没问题**的文件必须仍然不回落。
///
/// 这条兼作牙齿自证 —— 若检测函数恒真，它会立刻变红。
#[test]
fn d275_healthy_files_still_do_not_fall_back() {
    for name in [
        "arithmetic.mora",
        "for_loop.mora",
        "tea_app.mora",
        "tea_counter.mora",
    ] {
        let path = fixture(name);
        assert!(
            std::path::Path::new(&path).exists(),
            "fixture 不存在：{path}"
        );
        assert!(
            !nine_layer_fell_back(&path),
            "{name} 本就通过差分，不该回落 —— 检测函数可能恒真"
        );
    }
}

/// 回落不得改变可观察行为（退出码）。
///
/// 差分失败只丢弃 9 层产出、不改变结果 —— 这条钉住「回落是静默降级、
/// 不是崩溃」。
///
/// 只用 `tea_standalone`：它是失败集里**有 `task main()` 入口**的样本。
/// （`rel_empty.mora` 只有 `rel` + `solve`、**没有入口**，它 exit 1 是
/// 正确的、与差分无关 —— 别把它当基线。）
#[test]
fn d275_fallback_does_not_break_the_program() {
    let exe = env!("CARGO_BIN_EXE_mora");
    let out = Command::new(exe)
        .arg("run")
        .arg(fixture("tea_standalone.mora"))
        .output()
        .expect("应能执行 mora");
    assert!(
        out.status.success(),
        "tea_standalone 本应正常跑完 —— 回落不应改变退出码。实际：{:?}",
        out.status
    );
}

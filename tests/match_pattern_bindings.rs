//! v0.104.6 D165：match arm 体里的**模式绑定**在双向层被判成「未定义变量」（已修）。
//!
//! ## 缺陷：一段 parser 接受、运行期正常的程序被 `mora --check` 拒绝
//!
//! ```mora
//! let d = {a: 1, b: 2}
//! let r = match d with
//!   {a: x} -> print(x)     ← 报 Unbound variable 'x'
//!   _ -> print(0)
//! end
//! ```
//!
//! **HM 侧是对的**：`infer_match` 在推断 arm 体之前调 `add_pattern_bindings`
//! 把绑定注册进 env，所以体里的 `x` 能推断。
//!
//! **双向侧是错的**：`bidirectional.rs` 的 Phase D 直接
//! `synth(&arm.body)` / `check_against(&arm.body, …)`，**没有先注册绑定**
//! → `infer_expr` 报 `Unbound variable 'x'` → 被 `check_against` 包成
//! 「type inference failed: …」推进 errors。
//!
//! 而 D128 的去重键是 `(line, expected, actual)` —— **HM 侧根本没报错**，
//! 无键可匹配 → 这条**假错误**一路留在最终诊断里。
//!
//! ## 为什么此前无人发现
//!
//! 既有 fixture 的 match arm **从不在体里使用绑定**（`[a, b, c] -> print("three")`，
//! 绑了但不用），所以从��触发。
//!
//! ## 修法
//!
//! 双向层 Phase D 的两处（`synth` 与 `check_against`）照 HM 的
//! save → add → infer → restore 顺序补上 `add_pattern_bindings`
//! （并放开其可见性到 `pub(crate)`）。守卫 `g` 同样在绑定就位下递归 ——
//! `x when x > 0` 引用绑定。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn typeck(src: &str) -> Result<Vec<String>, String> {
    let (_f, w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = check_program_witnesses_bidirectional(&w);
    if errs.is_empty() {
        Ok(vec![])
    } else {
        Ok(errs.iter().map(|e| e.message.clone()).collect())
    }
}

fn run(src: &str) -> Result<String, String> {
    let (f, _w) = mora::parser_v3::ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let arc = std::sync::Arc::new(f);
    mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D165 主判据 ①：arm 体里**引用绑定**必须能过 typeck（dict 与 list 两种形态）。
#[test]
fn d165_match_arm_body_can_use_pattern_bindings() {
    for (label, src) in [
        (
            "dict binding",
            "let d = {a: 1}\nlet r = match d with\n  {a: x} -> print(x)\n  _ -> print(0)\nend\nprint(r)\n",
        ),
        (
            "dict rest",
            "let d = {a: 1, b: 2}\nlet r = match d with\n  {a: x, ..} -> print(x)\n  _ -> print(0)\nend\nprint(r)\n",
        ),
        (
            "list binding",
            "let p = [1, 2]\nlet r = match p with\n  [x, y] -> print(x + y)\n  _ -> print(0)\nend\nprint(r)\n",
        ),
        (
            "list rest binding",
            "let p = [1, 2, 3]\nlet r = match p with\n  [x, ..] -> print(x)\n  _ -> print(0)\nend\nprint(r)\n",
        ),
    ] {
        let errs = typeck(src).expect("语法应通过");
        assert!(
            errs.is_empty(),
            "[{label}] arm 体里使用模式绑定必须能过 typeck\
             （修复前报 `Unbound variable` —— HM 注册了绑定、双向层没注册）; 实得: {errs:?}"
        );
    }
}

/// D165 主判据 ②：绑定在**运行期**取到正确的值（不是 nil）。
///
/// ⚠ 用「arm 体即值」而不是 `print`：库里 `println!` 与被测程序的
/// stdout 会**交错**，我第一版探针因此把三种形态全看成 `nil`，
/// 差点误判普通变量模式也坏了（CLI 复核后证明它打 7.0，正常）。
#[test]
fn d165_match_arm_bindings_have_correct_runtime_values() {
    for (label, src, want) in [
        (
            "plain var",
            "let r = match 7 with\n  k -> k\n  _ -> 0\nend\nr\n",
            "7.0",
        ),
        (
            "list binding",
            "let r = match [1, 2] with\n  [a, b] -> a + b\n  _ -> 0\nend\nr\n",
            "3.0",
        ),
        (
            "dict binding",
            "let r = match {a: 9} with\n  {a: k} -> k\n  _ -> 0\nend\nr\n",
            "9.0",
        ),
    ] {
        let got = run(src).unwrap_or_else(|e| panic!("[{label}] 运行期不应报错: {e}"));
        assert_eq!(got, want, "[{label}] 绑定值应正确; 实得: {got}");
    }
}

/// D165 反向对照：不含绑用的 arm、以及既有诊断行为**不得**回退。
#[test]
fn d165_unrelated_match_behaviour_unchanged() {
    // 不使用绑定的 arm（既有 fixture 的常见形态）必须仍然零诊断
    let e = typeck(
        "let p = [1, 2, 3]\nlet r = match p with\n  [] -> print(\"e\")\n  [a, b, c] -> print(\"3\")\n  _ -> print(\"o\")\nend\nprint(r)\n",
    )
    .expect("compile");
    assert!(e.is_empty(), "不使用绑用的 arm 应零诊断; 实得: {e:?}");

    // arm 体类型不一致**仍须**被拒（Phase D 的 joined 检查不得被我的改动绕过）
    let bad =
        typeck("let r = match 1 with\n  1 -> 1\n  _ -> \"str\"\nend\nprint(r)\n").expect("compile");
    assert!(
        !bad.is_empty(),
        "arm 体类型不一致仍必须被拒（Phase D 的 joined 检查是承重的）; 实得: {bad:?}"
    );
}

/// **已知缺口**（非缺陷断言）：类型标注模式 `n: number` 彻底不可用。
///
/// 三层都错，且症状各不相同：
///
/// | 层 | 现象 |
/// |---|---|
/// | parser | `n: number` → `TypeAscription { name: "n", pattern: Variable("number") }` —— **名字与内层对调** |
/// | typeck | 两条看不懂的错：`Unbound variable 'n'` + `Type mismatch: expected known type name, got n` |
/// | 运行期 | **arm 永不匹配** —— 静默落到 `_` 默认分支（实测 `n: number -> n` 得 `0.0`、`x: string -> x` 得 `"no"`，应分别是 `5.0` / `"hi"`） |
///
/// 运行期那一条属**静默错误结果**（本会话最坏的一类）。修它要先定
/// 「`n: number` 到底该绑定谁、类型从哪来」—— 属语言设计问题，
/// 本轮只报告不实施（写成本条断言以免后人重新发现一遍）。
///
/// 若将来修好，本条会红 —— 届时请把 D165 的两条主判据扩到该形态。
#[test]
fn d165_type_ascription_pattern_is_a_known_gap() {
    // (1) 类型层：仍是不可理解的两条错
    let errs =
        typeck("let r = match 5 with\n  n: number -> print(n)\n  _ -> print(0)\nend\nprint(r)\n")
            .expect("语法应通过（parser 接受该形态）");
    assert!(
        !errs.is_empty(),
        "已知缺口：`n: number` 目前被 typeck 拒绝。若本条转红说明已修 —— \
         请把 D165 主判据扩到类型标注形态; 实得: {errs:?}"
    );

    // (2) 运行期：arm 永不匹配，静默落到默认分支（这是最严重的一面）
    assert_eq!(
        run("let r = match 5 with\n  n: number -> n\n  _ -> 0\nend\nr\n").as_deref(),
        Ok("0.0"),
        "已知缺口：`n: number -> n` 永不匹配，静默取 `_` 分支（应为 5.0）"
    );
    // ⚠ `Value::String` 的 Display **不带引号**（`no` 而非 `"no"`）
    assert_eq!(
        run("let r = match \"hi\" with\n  x: string -> x\n  _ -> \"no\"\nend\nr\n").as_deref(),
        Ok("no"),
        "已知缺口：`x: string -> x` 永不匹配，静默取 `_` 分支（应为 hi）"
    );
}

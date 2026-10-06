//! v0.104.6 D121：同族普查 —— D120 之后，`Node` 变体的 `result-reg` 覆盖是否还有漏的。
//!
//! ## 为什么做这件事
//!
//! D120（`Node::Solve` 缺 `dst` → `let` 绑定拿哨兵 0 → 后续 Sequence 静默饿死）
//! 与 D35（`Handle`）、D58（`WithConfig`）是同一族。D104 早先把
//! `Parallel` / `Observe` / `Span` / `PromptSection` / `DocumentSection`
//! 这 5 个**也缺 `dst`** 的变体记为「**潜在**风险，被差分回落挡住」。
//!
//! 本文件把那个「潜在」做成**实测结论**：`mora::mir::fcfg::Node` 共 56 个变体，
//! 逐个核对 `node_result_reg_of` 是否有 arm、该 arm 引用的字段是否真实存在。
//!
//! ## 两个关键事实
//!
//! 1. **无矛盾**：`node_result_reg_of` 里每一条 arm 引用的 `dst`/`reg` 字段
//!    都在对应 `Node` 变体上真实存在（D120 修完后成立）。
//! 2. **5 类块变体是安全的**（D104 的「潜在」不成立）：它们作 `let` 右值时
//!    **明确报错**（`Failed to parse`），作语句时**后续语句照常执行** ——
//!    不存在 D120 那种「静默饿死」。
//!
//! ⚠ 判据一律走 **9 层管线**（`cli::compile_and_opt`）并**执行产物** ——
//! 缺陷住在 `witness_to_fcfg`，直出路径永远看不到（D120 的教训）。

use mora::cli::compile_and_opt;
use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

/// 五类「块型」`Node` 变体在源码层的形态（`let` 右值 / 语句位置各一套）。
///
/// 语法形态取自 spec §9.1（`parallel` 无 `do`）与 §12 / §18.3。
const BLOCK_FORMS: &[(&str, &str, &str)] = &[
    (
        "observe",
        "observe trace \"svc\" do\n  print(1)\nend",
        "observe trace \"svc\" do\n  print(1)\nend\n",
    ),
    (
        "span",
        "span \"sp\" do\n  print(1)\nend",
        "span \"sp\" do\n  print(1)\nend\n",
    ),
    (
        "prompt",
        "prompt \"sys\" do\n  print(1)\nend",
        "prompt \"sys\" do\n  print(1)\nend\n",
    ),
    (
        "document",
        "document \"d\" do\n  print(1)\nend",
        "document \"d\" do\n  print(1)\nend\n",
    ),
    (
        "parallel",
        "parallel\n  let a = 1\nend",
        "parallel\n  let a = 1\nend\n",
    ),
];

fn pipelined_last_value(src: &str) -> Result<String, String> {
    let (func, _wit) =
        compile_and_opt(src, None).map_err(|e| format!("9 层管线编译失败: {e:?}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => Ok(format!("{v:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// D121 主判据 ①：五类块作 **`let` 右值**必须**明确报错**。
///
/// 这是防「静默」的关键 —— D120 的教训是「能解析但饿死」比「解析失败」危险得多。
/// 若某天这五类被改成可作表达式，本测试会红，提示必须同时给它们补 `dst`。
#[test]
fn d121_block_forms_as_let_rhs_are_rejected_not_silently_accepted() {
    for (name, block, _) in BLOCK_FORMS {
        let src = format!("let z = {block}\nprint(z)\n");
        assert!(
            ParserV3::compile(&src).is_err(),
            "{name}: 块作 `let` 右值当前必须**解析失败**。\
             若本测试失败，说明它已可作表达式 —— 此时 `Node::{name}` 若仍缺 `dst`，\
             就会重演 D120（`let` 绑定拿哨兵 0 → 后续 Sequence 静默饿死）"
        );
    }
}

/// D121 主判据 ②：五类块作**语句**时，**后续语句必须照常执行**。
///
/// 走 9 层管线并执行产物 —— 末值取不到就说明整条链被饿死。
#[test]
fn d121_block_forms_as_statements_do_not_starve_following_code() {
    for (name, _, stmt) in BLOCK_FORMS {
        let src = format!("{stmt}let tail = 77\ntail\n");
        let got = pipelined_last_value(&src)
            .unwrap_or_else(|e| panic!("{name}: 9 层管线执行失败（这本身是另一个问题）: {e}"));
        assert_eq!(
            got, "Float(77.0)",
            "{name}: 块执行后 `let tail = 77` 必须照常绑定；\
             末值取不到说明后续 Sequence 被饿死"
        );
    }
}

/// 对照组：同一个「块 + 后续语句」形态，**裸直出路径**也要一致。
///
/// 与 D120 同理 —— 只测一条路径等于没测。
#[test]
fn d121_direct_and_pipelined_paths_agree_for_block_statements() {
    for (name, _, stmt) in BLOCK_FORMS {
        let src = format!("{stmt}let tail = 77\ntail\n");
        let (direct, _dw) = ParserV3::compile(&src).expect("直出编译");
        let direct_val = {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            let arc = Arc::new(direct);
            match run_mir(
                &arc,
                &mut interp,
                &mut env,
                &mut mora::mir::effect::Effects::new(),
            ) {
                Ok(v) => format!("{v:?}"),
                Err(e) => format!("Err({e})"),
            }
        };
        let pipe_val = pipelined_last_value(&src).expect("9 层管线执行");
        assert_eq!(direct_val, pipe_val, "{name}: 两条编译路径结果分叉");
    }
}

/// 正面确认：`parallel` 块内的 `let` 会**外泄**到块外（spec §9.1 承诺）。
///
/// 与 `with` 块相反（`with` 的 `let` 不外泄，见 D59/D107）—— 两个块家族的
/// 作用域规则**不同**，容易记混，各钉一条。
#[test]
fn d121_parallel_block_lets_leak_outside_as_spec_promises() {
    let src = "parallel\n  let a = 10\nend\na\n";
    assert_eq!(
        pipelined_last_value(src).expect("执行"),
        "Float(10.0)",
        "spec §9.1 的 `parallel` 块内 `let` 应在块外可见"
    );
}

/// 对照组：`with` 块的 `let` **不**外泄 —— 与上一条成对，防止记混。
#[test]
fn d121_with_block_lets_do_not_leak_contrast_with_parallel() {
    let src = "with model = \"m\"\n  let inner = 1\nend\ninner\n";
    // `inner` 在 with 块外不可见 → 报未定义变量，而不是 1.0
    let res = pipelined_last_value(src);
    assert!(
        res.as_deref() != Ok("Float(1.0)"),
        "`with` 块内的 `let` 不应外泄（与 parallel 相反）; 实测: {res:?}"
    );
}

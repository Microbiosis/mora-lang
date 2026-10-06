//! `continue` / `break` 语义的三路对照 —— 判定「哪条路径才是对的」。
//!
//! ## 为什么需要这个文件
//!
//! `tests/run_mir_equiv_run_dag.rs` 用「生产路径 vs 强制线性路径」比较，
//! 并在文件头断言 `while + continue` 用例里**生产路径的 5.0 是错的、
//! 强制线性的 6.0 是对的**，还把它归因为执行器缺陷 E1。
//!
//! v0.104.6 复核发现**两处都错**（详见该文件头的「更正记录」）：
//! 正确的一侧搞反了，且 E1 已修复而发散依旧、归因不成立。
//!
//! 危险在于：一个被文档断言为「错」的正确结果，会让人去查一个**并不存在**
//! 的缺陷；而「哪条路径对」这种问题不能靠读代码拍板，必须有**独立于那两条
//! 路径**的判据。
//!
//! ## 判据：用 `continue` 的语义等价改写做交叉验证
//!
//! `continue` 跳过循环体剩余部分。把它改写成语义等价的 `if/else`（跳过与
//! 不跳过分别放进两支），应当得到**完全相同**的结果；而把 `continue` 整个
//! 删掉，则应当恰好多出被跳过的那些次。三者交叉即可独立判定对错。

use mora::interpreter::Interpreter;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// 带 `continue`：`k` 走到 2 时跳过一次 `n` 自增。正确答案 = 5。
const WITH_CONTINUE: &str = "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n    continue\n  end\n  assign n = n + 1\n  assign k = k + 1i\nend\nn\n";

/// `continue` 的 if/else 等价写法：跳过与不跳过显式分进两支。正确答案 = 5。
const CONTINUE_AS_IF_ELSE: &str = "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n  else\n    assign n = n + 1\n    assign k = k + 1i\n  end\nend\nn\n";

/// 删掉 `continue`（连同整个 if）：六轮全部执行。正确答案 = 6。
const WITHOUT_CONTINUE: &str =
    "let n = 0\nlet k = 0i\nwhile k < 6i\n  assign n = n + 1\n  assign k = k + 1i\nend\nn\n";

/// `continue` 与其 if/else 等价写法必须给出**同一个值** —— 这是独立于
/// 「生产路径 vs 强制线性路径」的第三方判据。若两者不一致，说明
/// `continue` 本身就没被正确实现，等价改写不能用来给它背书。
#[test]
fn production_path_honours_continue() {
    let with = run(WITH_CONTINUE);
    let equivalent = run(CONTINUE_AS_IF_ELSE);
    assert_eq!(
        with, equivalent,
        "`continue` 与其 if/else 等价写法结果必须一致（实测 {with} vs {equivalent}）"
    );
    assert_eq!(
        with, "Float(5.0)",
        "k 走到 2 时跳过一次 n 自增，6 轮里应自增 5 次"
    );
}

/// 删掉 `continue` 后应恰好多出被跳过的那些次 —— 反向确认上一步的 5.0
/// 是「跳过一次」而非「少跑了别的什么」。
#[test]
fn removing_continue_adds_exactly_the_skipped_iteration() {
    assert_eq!(
        run(WITHOUT_CONTINUE),
        "Float(6.0)",
        "无 continue 时六轮全部自增"
    );
    assert_eq!(run(WITH_CONTINUE), "Float(5.0)", "有 continue 时少一次");
}

/// `break` 的同款对照：`x` 到 3 时跳出，`n` 应停在 2。
#[test]
fn break_semantics() {
    let with_break = run(
        "let n = 0\nfor x in [1, 2, 3, 4, 5]\n  if x == 3 then\n    break\n  end\n  assign n = n + 1\nend\nn\n",
    );
    assert_eq!(
        with_break, "Float(2.0)",
        "x=3 时跳出，此前只自增了 1、2 两次"
    );
    let without_break = run("let n = 0\nfor x in [1, 2, 3, 4, 5]\n  assign n = n + 1\nend\nn\n");
    assert_eq!(without_break, "Float(5.0)", "无 break 时五轮全部自增");
}

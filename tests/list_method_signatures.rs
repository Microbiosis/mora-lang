//! v0.104.6 D130：9 个 List 方法**运行期已实现、typeck 无签名** → 假阴性。
//!
//! ## 缺陷
//!
//! spec §12 方法表承诺这些 List 方法，运行期也确实实现了（全部实测可用），
//! 但 `typeck::dispatch::method_signature_builtin` 里**没有它们的签名**。
//! 无签名 → 返回未解算的 `TypeVar` → 与**任何**类型标注都「相容」→
//! **类型标注形同虚设**：
//!
//! ```mora
//! let xs = [1, 2, 3]
//! let bad: string = xs.take(2)      // 修复前：exit 0，零诊断
//! print(bad)                        // 运行时 bad 是个 list，不是 string
//! ```
//!
//! 与 D54（`let n: String = d.len()` 静默被接受）同型。
//!
//! ## 补的签名（9 条）
//!
//! `take` / `drop` / `window` / `batch` / `reduce` / `reshape` / `transpose` /
//! `flatten` / `shape`。返回类型尽量保留元素类型 `elem`（与既有
//! `map`/`filter`/`push` 同做法）；元素类型本身不可知的用 `Any`
//! （`flatten` 的内层可以是异质，如 `[[1],["x"]]`）。

use mora::cli::compile_and_opt;
use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use std::sync::Arc;

fn diagnostics(src: &str) -> Vec<String> {
    let (_f, wits) = compile_and_opt(src, None).expect("应能编译");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

fn run(src: &str) -> String {
    let (func, _w) = compile_and_opt(src, None).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    format!(
        "{}",
        run_mir(
            &arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new()
        )
        .expect("run")
    )
}

/// ⚠ **主判据（假阴性）**：错误的类型标注**必须被拒**。
///
/// 修复前这三条全部 exit 0、零诊断 —— 标注完全没起作用。
#[test]
fn d130_wrong_annotations_on_list_methods_are_rejected() {
    for (name, src, needle) in [
        (
            "take_as_string",
            "let xs = [1, 2, 3]\nlet bad: string = xs.take(2)\nprint(bad)\n",
            "String",
        ),
        (
            "reduce_as_string",
            "let xs = [1, 2, 3]\nlet bad: string = xs.reduce(fn(a, b) a + b end, 0)\nprint(bad)\n",
            "String",
        ),
        (
            "flatten_as_number",
            "let xs = [[1, 2], [3]]\nlet bad: number = xs.flatten()\nprint(bad)\n",
            "Float",
        ),
        (
            "shape_as_string",
            "let xs = [[1, 2], [3]]\nlet bad: string = xs.shape()\nprint(bad)\n",
            "String",
        ),
    ] {
        let errs = diagnostics(src);
        assert!(
            !errs.is_empty(),
            "[{name}] 错误的类型标注必须被 typeck 拒绝（修复前是**假阴性**：\
             无签名 → TypeVar → 与任何标注相容 → 标注形同虚设）; 实际 0 条诊断"
        );
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "[{name}] 诊断应点明期望类型含 {needle}; 实际: {errs:?}"
        );
    }
}

/// 反向对照：正确的标注**必须仍通过** —— 防「补签名引入假阳性」。
///
/// 这条与上一条成对：补签名既不能放过错的，也不能拒掉对的。
#[test]
fn d130_correct_annotations_on_list_methods_are_accepted() {
    for (name, src) in [
        (
            "take_as_list",
            "let xs = [1, 2, 3]\nlet ys: list<number> = xs.take(2)\nprint(ys)\n",
        ),
        (
            "reduce_as_number",
            "let xs = [1, 2, 3]\nlet n: number = xs.reduce(fn(a, b) a + b end, 0)\nprint(n)\n",
        ),
        (
            "flatten_as_list",
            "let xs = [[1, 2], [3]]\nlet ys: list<number> = xs.flatten()\nprint(ys)\n",
        ),
        (
            "window_as_nested_list",
            "let xs = [1, 2, 3]\nlet w: list<list<number>> = xs.window(2)\nprint(w)\n",
        ),
        (
            "drop_as_list",
            "let xs = [1, 2, 3]\nlet ys: list<number> = xs.drop(1)\nprint(ys)\n",
        ),
    ] {
        let errs = diagnostics(src);
        assert!(
            errs.is_empty(),
            "[{name}] 正确的类型标注必须被接受（补签名不得引入假阳性）; 实际: {errs:?}"
        );
    }
}

/// 反向对照：无标注的普通调用**照常工作**（补签名不得影响既有路径）。
#[test]
fn d130_unannotated_calls_still_run_correctly() {
    // 全部实测过的运行期行为，作为「补签名只动 typeck、不动运行期」的证据
    assert_eq!(
        run("let xs = [1, 2, 3]\nxs.reduce(fn(a, b) a + b end, 0)\n"),
        "6.0"
    );
    assert_eq!(run("let xs = [1, 2, 3]\nxs.take(2)\n"), "[1.0, 2.0]");
    assert_eq!(run("let xs = [1, 2, 3]\nxs.drop(1)\n"), "[2.0, 3.0]");
    assert_eq!(
        run("let xs = [1, 2, 3]\nxs.window(2)\n"),
        "[[1.0, 2.0], [2.0, 3.0]]"
    );
    assert_eq!(
        run("let xs = [1, 2, 3]\nxs.batch(2)\n"),
        "[[1.0, 2.0], [3.0]]"
    );
    assert_eq!(
        run("let xs = [[1, 2], [3]]\nxs.flatten()\n"),
        "[1.0, 2.0, 3.0]"
    );
    assert_eq!(run("let xs = [[1, 2], [3, 4]]\nxs.shape()\n"), "[2.0, 2.0]");
    assert_eq!(
        run("let xs = [[1, 2], [3, 4]]\nxs.transpose()\n"),
        "[[1.0, 3.0], [2.0, 4.0]]"
    );
}

/// `flatten` 的返回元素类型是 `Any`（内层元素类型不可知），
/// 因此**具体元素类型的标注**也必须被接受 —— 不能要求用户先知道内层是什么。
///
/// ⚠ 两点注意：
/// - 想构造「内层异质」的输入（`[[1],["x"]]`）**做不到**：它本身违反 D125 的
///   list 字面量同质约束，编译期就报同质错。故本条不测异质输入。
/// - 裸 `list` 标注**不被支持**（`unsupported type annotation 'list'`，既有行为，
///   与本轮无关）—— 故此处用 `list<number>`。
#[test]
fn d130_flatten_result_accepts_a_concrete_element_annotation() {
    let errs =
        diagnostics("let xs = [[1, 2], [3]]\nlet ys: list<number> = xs.flatten()\nprint(ys)\n");
    assert!(
        errs.is_empty(),
        "`flatten` 的返回元素类型是 Any（内层不可知），`list<number>` 标注必须被接受; 实际: {errs:?}"
    );
}

/// `reduce` 的初值必须与元素同类型 —— 签名里 `init` 绑成 `elem`。
#[test]
fn d130_reduce_init_must_match_element_type() {
    // 合法：init 是 number
    assert!(
        diagnostics(
            "let xs = [1, 2, 3]\nlet n: number = xs.reduce(fn(a, b) a + b end, 0)\nprint(n)\n"
        )
        .is_empty()
    );
    // 非法：init 是 string，与元素 number 不符
    let errs = diagnostics("let xs = [1, 2, 3]\nxs.reduce(fn(a, b) a + b end, \"\")\n");
    assert!(
        !errs.is_empty(),
        "`reduce` 的初值必须与元素同类型（`[1,2,3].reduce(f, \"\")` 是错的）"
    );
}

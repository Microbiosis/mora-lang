//! v0.104.6 D153：普查范围漏了**值方法面** —— `list.get` 的 `Int` 索引静默变 0（已修）。
//!
//! ## 为什么 D150/D152 没抓到
//!
//! 那两轮的普查关键字是 `args.get(` 且路径限 `src/interpreter/builtins/` ——
//! 那是**命名空间函数**面。而 `xs.get(i)` / `xs.take(n)` / `xs.reshape(r,c)` /
//! `xs.crush_json(max)` 走的是**值方法**面（`method_dispatch.rs`），
//! 写法是 `.and_then(|v| match v { Value::Float(n) => …, _ => None })`。
//!
//! **教训：普查的范围比缺陷的机制更窄时，绿灯是假的。**
//!
//! ## 最严重的一处：`list.get`
//!
//! ```text
//! let xs = [10, 20, 30]
//! xs.get(2)             → 30.0                       ✅
//! xs.get(9)             → ERR index 9 out of bounds ✅
//! xs.get(len([9, 9]))   → 10.0  ❌（Int 索引静默变 0 → 返回首元素）
//! xs.get(len([9,9,9]))  → 10.0  ❌（索引 3 越界，本该报错，却静默返回首元素）
//! ```
//!
//! 第二条尤其恶劣：**一个本该报错的输入静默返回了首元素** ——
//! 比 D152 的 `plan.list`（用户拿到「另一个操作」的结果）更糟，连报错都被吞掉。
//!
//! ## 同族其余：归因错误的报错
//!
//! `take` / `drop` / `window` / `batch` / `reshape` / `crush_json` 传 `Int` 时报
//! 「requires a count argument」—— **实参明明传了**，只是个合法的 `Int`。
//!
//! ## 顺带更正 D148 普查表的**两处错判**
//!
//! D148 的饱和转换普查表把 `crush_json(max)` 与 `tail(max)` 记为
//! 「本就正确 · 作样板」。**那是错的** —— 那次只审了**负数**一侧，
//! 没审**类型**一侧：两者都只匹配 `Float`，`Int` 实参同样被拒。

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).map(|v| format!("{v}"))
}

/// D153 主判据 ①：`list.get` 的 `Int` 索引必须**指向那个元素**，不是静默取第 0 个。
#[test]
fn d153_list_get_honours_int_index() {
    let out = run("let xs = [10, 20, 30]\nlet i = len([9, 9])\nxs.get(i)\n")
        .unwrap_or_else(|e| panic!("Int 索引不应报错: {e}"));
    assert_eq!(
        out, "30.0",
        "`xs.get(len([9,9]))` 应得 xs[2]=30.0（修复前静默得 10.0）"
    );
}

/// D153 主判据 ②：越界的 `Int` 索引必须**报错**，不能静默返回首元素。
///
/// 这是本轮最关键的一条：修复前一个**本该失败**的输入安静地返回了 `xs[0]`。
#[test]
fn d153_list_get_out_of_bounds_int_index_errors() {
    let res = run("let xs = [10, 20, 30]\nlet i = len([9, 9, 9])\nxs.get(i)\n");
    let e = res.expect_err("索引 3 越界（len 3）必须报错");
    assert!(
        e.contains("out of bounds"),
        "越界必须报越界（修复前静默返回 xs[0]）; 实得: {e}"
    );
}

/// D153 主判据 ③：`take`/`drop`/`window`/`batch`/`reshape` 必须认 `Int`。
#[test]
fn d153_list_methods_accept_int_counts() {
    let cases = [
        ("xs.take(i)", "[1.0, 2.0]"),
        ("xs.drop(i)", "[3.0, 4.0, 5.0, 6.0, 7.0, 8.0]"),
        (
            "xs.batch(i)",
            "[[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]]",
        ),
    ];
    for (call, want) in cases {
        let out = run(&format!(
            "let xs = [1, 2, 3, 4, 5, 6, 7, 8]\nlet i = len([1, 1])\n{call}\n"
        ))
        .unwrap_or_else(|e| panic!("[{call}] Int 实参不应报错: {e}"));
        assert_eq!(out, want, "[{call}] Int 实参必须与 Float 字面量等价");
    }
    // window：长度可自行核对
    let w = run("let xs = [1, 2, 3, 4]\nlet s = len([1, 1])\nxs.window(s)\n")
        .unwrap_or_else(|e| panic!("[window] Int 实参不应报错: {e}"));
    assert_eq!(
        w, "[[1.0, 2.0], [2.0, 3.0], [3.0, 4.0]]",
        "[window] Int 实参必须与 Float 等价"
    );

    let r = run("let g = [[1, 2, 3, 4], [5, 6, 7, 8]]\n\
         let a = len([1, 1])\n\
         let b = len([1, 1, 1, 1])\n\
         g.reshape(a, b)\n")
    .unwrap_or_else(|e| panic!("[reshape] Int 实参不应报错: {e}"));
    assert_eq!(
        r, "[[1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]]",
        "[reshape] 两个 Int 实参都必须与 Float 等价"
    );
}

/// D153 主判据 ④：D148 普查表里被误判为「本就正确」的 `crush_json` / `tail` 必须认 `Int`。
#[test]
fn d153_crush_json_and_tail_accept_int_max() {
    let c = run("let xs = [1, 2, 3, 4, 5, 6, 7, 8]\nlet n = len([1, 1])\nxs.crush_json(n)\n")
        .unwrap_or_else(|e| panic!("crush_json 的 Int max 不应报错: {e}"));
    assert!(
        c.contains("items=2"),
        "crush_json 的 Int max 必须与 Float 等价（items=2）; 实得: {c}"
    );

    let dir = std::env::temp_dir().join("mora_d153_tail");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let f = dir.join("t.txt");
    std::fs::write(&f, "l1\nl2\nl3\nl4\n").expect("写文件");
    let p = f.display().to_string().replace('\\', "/");
    let t = run(&format!("let n = len([1, 1])\ntail(\"{p}\", n)\n"))
        .unwrap_or_else(|e| panic!("tail 的 Int max 不应报错: {e}"));
    assert!(
        !t.is_empty(),
        "tail 的 Int max 必须与 Float 等价; 实得: {t}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D153 主判据 ⑤：错类型必须报**类型错**，不得报「没传」（归因正确）。
///
/// ⚠ **可达性说明（D154 更正）**：本文件用 `ParserV3::compile` + `run_mir`，
/// **绕过 typeck**。而真实 CLI（`mora <file>` / `mora --check`）会先跑 typeck，
/// 同一段程序在**用户手上一条都到不了运行期**：
///
/// ```text
/// xs.take("two")
///   CLI 路径 → exit 2，Type mismatch: expected int, got string（typeck 拦下）
///   本文件   → 运行期 "take: count must be a number"
/// ```
///
/// 即：修复后的运行期消息是**正确的**，但它只在**不过 typeck 的入口**
/// （库 API / REPL / LSP 运行时）可见。保留本条是为了钉住那条路径的
/// 归因正确性；配套的 `d154_cli_rejects_wrong_typed_arg_before_runtime`
/// 钉住 CLI 路径的行为。两条合起来才是不完整的反面。
#[test]
fn d153_wrong_type_reports_type_error_not_missing_argument() {
    for (call, what) in [
        ("[1,2,3].take(\"two\")", "count"),
        ("[1,2,3].drop(\"two\")", "count"),
        ("[1,2,3].window(\"two\")", "size"),
        ("[1,2,3].batch(\"two\")", "size"),
        ("[1,2,3].get(\"two\")", "index"),
        ("[1,2,3].crush_json(\"two\")", "max"),
    ] {
        let e = run(&format!("{call}\n")).expect_err(&format!("{call} 传字符串应报错"));
        assert!(
            e.contains("must be a number"),
            "[{call}] 应报**类型错** `must be a number`（修复前报 `requires …`，归因错误）; 实得: {e}"
        );
        assert!(
            e.contains(what),
            "[{call}] 错误信息应点名 `{what}`; 实得: {e}"
        );
        assert!(
            !e.contains("requires"),
            "[{call}] 实参**传了**，不该说 requires（归因错误）; 实得: {e}"
        );
    }
}

/// D153 反向对照：Float 字面量路径必须**逐字**不变。
#[test]
fn d153_float_paths_unchanged() {
    for (src, want) in [
        ("let xs = [10, 20, 30]\nxs.get(2)\n", "30.0"),
        (
            "let xs = [1, 2, 3, 4, 5, 6, 7, 8]\nxs.take(2)\n",
            "[1.0, 2.0]",
        ),
        (
            "let xs = [1, 2, 3, 4, 5, 6, 7, 8]\nxs.drop(6)\n",
            "[7.0, 8.0]",
        ),
        (
            "let xs = [1, 2, 3, 4]\nxs.window(2)\n",
            "[[1.0, 2.0], [2.0, 3.0], [3.0, 4.0]]",
        ),
    ] {
        let out = run(src).unwrap_or_else(|e| panic!("[{src}] 不应报错: {e}"));
        assert_eq!(out, want, "[{src}] Float 字面量路径必须逐字不变");
    }
    // Float 越界仍照旧报错
    let e = run("let xs = [10, 20, 30]\nxs.get(9)\n").expect_err("Float 越界应报错");
    assert!(
        e.contains("out of bounds"),
        "Float 越界报错不得回退; 实得: {e}"
    );
}

/// D153 对照组：D145/D146 的**负数**守卫不得回退。
#[test]
fn d153_negative_guards_still_hold() {
    for src in [
        "[1, 2, 3].take(-1)\n",
        "[1, 2, 3].drop(-1)\n",
        "[1, 2, 3].get(-1)\n",
        "[1, 2, 3].crush_json(-1)\n",
    ] {
        let e = run(src).expect_err(&format!("{src} 负数必须报错"));
        assert!(
            e.contains("不能为负数")
                || e.contains("negative")
                || e.contains("> 0")
                || e.contains("non-negative"),
            "[{src}] 负数应被明确拒绝; 实得: {e}"
        );
    }
}

/// D153 对照组：同族里**本来就正确**的三处不得回退。
///
/// `range` / `random.rand_int` / `stats.histogram` 在更早的轮次里已修好，
/// 它们是这一族的**可触达正面样板** —— 钉住它们以防「统一」时被改坏。
#[test]
fn d153_already_correct_siblings_not_regressed() {
    for src in [
        "let n = len([1, 1, 1, 1, 1])\nlen(range(0, n))\n",
        "let a = random.rand_int(len([0, 1]), len([0, 1, 1]))\ntype_of(a)\n",
        "let h = stats.histogram([1.0, 2.0, 3.0], len([1, 1]))\nlen(h)\n",
    ] {
        let res = run(src);
        if let Err(e) = res {
            // 这些是 happy path，允许因其它原因失败，但不得是「Int 实参被拒」
            assert!(
                !e.contains("must be a number") && !e.contains("requires"),
                "[{src}] 本就正确的 Int 路径被误伤; 实得: {e}"
            );
        }
    }
}

/// D154：钉住**真实 CLI 路径**（带 typeck）对错类型实参的行为。
///
/// 本文件其余判据用 `ParserV3::compile` + `run_mir` 绕过 typeck；
/// 而 `mora <file>` / `mora --check` **会**先跑 typeck，在那里错类型实参
/// 一条都到不了运行期。本条用 `cli::compile_and_opt` +
/// `check_program_witnesses_bidirectional` 复现真实链路。
///
/// 它的存在理由是**让 D153 的 ⑤ 不再自欺**：那条断言的运行期消息，
/// 用户在 CLI 上永远看不到。
#[test]
fn d154_cli_rejects_wrong_typed_arg_before_runtime() {
    let typeck = |src: &str| -> Result<(), String> {
        let (_f, w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
        let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&w);
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join(" | "))
        }
    };
    for call in [
        "[1,2,3].take(\"two\")",
        "[1,2,3].drop(\"two\")",
        "[1,2,3].window(\"two\")",
        "[1,2,3].batch(\"two\")",
        "[1,2,3].get(\"two\")",
    ] {
        let e = typeck(&format!("let xs = [1, 2, 3]\nlet r = {call}\nprint(r)\n"))
            .expect_err(&format!("[{call}] CLI 路径应在 typeck 拦下"));
        assert!(
            e.contains("Type mismatch"),
            "[{call}] 应在 typeck 层被拒; 实得: {e}"
        );
    }
    // ⚠ `crush_json` 是**已知例外**，不参与上面的「typeck 拦下」清单：
    // `typeck/hm/builtin.rs:143` 把它的第 2/3 参声明成 fresh TypeVar，
    // 约束无信息量 → **typeck 放行任何类型**（D130 同族的假阴性）。
    // 因此 `xs.crush_json("two")` 在 CLI 上**真的会走到运行期**，
    // D153 修好的那条类型报错对它是**用户可见**的。
    typeck("let xs = [1, 2, 3]\nlet r = xs.crush_json(\"two\")\nprint(r)\n")
        .expect("`crush_json` 的 max 形参是 TypeVar —— typeck 放行（已知假阴性）");
    // 反向对照：合法用法（含 `Int` 实参）必须**照常通过** typeck
    for call in [
        "xs.take(len([1, 1]))",
        "xs.drop(len([1, 1]))",
        "xs.window(len([1, 1]))",
        "xs.batch(len([1, 1]))",
        "xs.get(len([1, 1]))",
        "xs.crush_json(len([1, 1]))",
    ] {
        typeck(&format!("let xs = [1, 2, 3]\nlet r = {call}\nprint(r)\n"))
            .unwrap_or_else(|e| panic!("[{call}] 合法用法不得被 typeck 拒; 实得: {e}"));
    }
}

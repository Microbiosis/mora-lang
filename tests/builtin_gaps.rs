//! v0.104.6：**「typeck 登记了签名、运行期没有实现」与「静默降级」两族缺陷的护栏**。
//!
//! 本文件锁住四个此前从未被任何测试触及过的缺陷。它们共同的性质是
//! **不报错**：`range(0, len(xs))` 求和得 `0.0` 而不是报错退出，
//! `int("42")` 这种则连调用都写不出来（因为全仓库没人用过）。
//! 也就是说，**旧测试全绿并不能说明这里是对的**。
//!
//! ## 四个缺陷
//!
//! | # | 现象 | 修前实测 | 修后 | 根因位置 |
//! |---|------|----------|------|----------|
//! | D1 | `range` 实参非 `Float` 时**静默取默认值** | `for i in range(0, len(xs))` 求和 = **0.0**（应 15.0） | 15.0 | `builtin_impls.rs::call_builtin_range` |
//! | D2 | 字符串 `len()` 数 **UTF-8 字节** | `len("中文字")` = **9** | 3 | `call_builtin_len` / `method_dispatch.rs` |
//! | D3 | 字符串索引 `s[i]` **源码层完全不可用** | `s[0]` → `ERR: cannot index String("abc") with Float(0.0)` | `Char('a')` | `mir/vm.rs::index_value` |
//! | D4 | `int()` / `float()` / `bool()` 只存在于 typeck | `ERR: Undefined function or task: int` | `Int(42)` | `interpreter/dispatch.rs` |
//!
//! ## D1 为什么最危险
//!
//! 本语言**所有数值字面量都是 `Float`**（`str(3)` 得 `"3.0"` 可证），
//! 而 `len()` / `xs.len()` 返的却是 `Int`。`call_builtin_range` 的三个实参
//! 各自 `.and_then(|v| match v { Value::Float(n) => …, _ => None }).unwrap_or(默认)`，
//! 于是 `Int` 实参被当成「没传」：
//!
//! - `end` 退回 `start` → range 恒空 → **循环体一次都不执行**
//! - `sum` 保持初值 `0.0` → **无报错、退出码 0**
//!
//! 也就是说，「求一个列表的和」这种最基础的程序写不出正确结果，只要它用了
//! `len()` 写循环上界。同一段代码把 `len(xs)` 换成字面量 `5` 立刻正确 ——
//! 这种「换个写法就对」的错误最难自查。
//!
//! 修法不是补 `Int` 一个洞，而是**消除静默降级这一失败模式本身**：
//! 实参存在但类型不对一律报错，只有实参真的缺席才用默认值。
//!
//! ## D4 的来历
//!
//! 与 v0.104.6 修掉的 `str()` **完全同源**：`typeck/hm/builtin.rs:48-56` 与
//! `typeck/dispatch.rs:177-189` 登记了 `int` / `float` / `bool` 的签名，
//! 于是任何用到它们的程序都能过类型检查；运行期 `dispatch.rs` 的
//! `match name` 里却没有对应分支，落到兜底的环境查找报
//! `Undefined function or task: …`。`str()` 之所以还多藏了一阵，是被
//! 执行器缺陷 E1 连带掩盖（见 `builtin_impls.rs::call_builtin_str` 的注释）；
//! `int/float/bool` 则是从一开始就没有任何测试碰过。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 跑一段 Mora 源码，返回 `Value` 的 Debug 形态或 `ERR: …`。
///
/// **同时跑 typeck** —— 本文件的断言都必须是「用户真写得出来」的代码。
/// 只用 `run_mir` 会绕过 typeck，于是「typeck 拒绝、运行期却支持」的写法
/// 会被当成有效断言固化。v0.104.6 在 `numeric_tower.rs` 上实测：加上这个
/// 守卫后 15 条测试里 **8 条当场失败**，全在断言不可达代码。
/// 需要记录「运行期已有、typeck 未放行」的能力时用 `run_runtime_only()`。
fn run(src: &str) -> String {
    let (func, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}\n{src}"));
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(
        errs.is_empty(),
        "这段源码被 typeck 拒绝，本测试却在断言它的行为 —— 断言的是用户写不出来的代码。\n\
         源：{src}\n错误：{:?}",
        errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// 只跑运行期、**故意绕过 typeck** —— 专用于记录「运行期已支持、typeck 尚未
/// 放行」的能力。**它不能证明「用户写得出来」。**
fn run_runtime_only(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}\n{src}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// 逐条断言 `(用例名, 源码, 期望输出)`。
fn expect_all(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        let got = run(src);
        if got != *want {
            failures.push(format!(
                "  [{name}]\n    src = {src:?}\n    want = {want}\n    got  = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 条不符：\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// D1：range 的静默降级
// ─────────────────────────────────────────────────────────────────────

#[test]
fn d1_range_bound_from_len_is_not_silently_empty() {
    // 三种写法写同一个循环上界，必须给出同一个和。
    // 修前：`len(xs)` 与 `xs.len()` 两版都得 0.0，字面量版得 15.0。
    let canonical = [
        (
            "free len()",
            "let xs=[1,2,3,4,5]\nlet s=0\nfor i in range(0, len(xs))\n  assign s = s + xs[i]\nend\ns\n",
        ),
        (
            "method .len()",
            "let xs=[1,2,3,4,5]\nlet s=0\nfor i in range(0, xs.len())\n  assign s = s + xs[i]\nend\ns\n",
        ),
        (
            "literal bound",
            "let xs=[1,2,3,4,5]\nlet s=0\nfor i in range(0, 5)\n  assign s = s + xs[i]\nend\ns\n",
        ),
    ];
    let mut failures = Vec::new();
    for (name, src) in canonical {
        let got = run(src);
        if got != "Float(15.0)" {
            failures.push(format!("  [{name}] want Float(15.0), got {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "「用 len() 写循环上界」与「用字面量写上界」结果发散 —— 静默降级：\n{}",
        failures.join("\n")
    );
}

#[test]
fn d1_range_over_dict_values_and_strings() {
    expect_all(&[
        (
            "len(range by list len)",
            "len(range(0, len([1,2,3])))\n",
            "Int(3)",
        ),
        (
            "len(range by str len)",
            "len(range(0, len(\"abcd\")))\n",
            "Int(4)",
        ),
        (
            "len(range by dict len)",
            "len(range(0, len({a:1,b:2})))\n",
            "Int(2)",
        ),
        // Int 实参现在被接受（`int()` 产出 Int）
        (
            "len(range by int())",
            "len(range(0, int(\"4\")))\n",
            "Int(4)",
        ),
    ]);
}

#[test]
fn d1_range_negative_step_works() {
    // 修前 `while i < end` 对负步长恒假 → 静默返回空列表。
    expect_all(&[
        (
            "neg step 3..0",
            "range(3, 0, -1)\n",
            "List([Float(3.0), Float(2.0), Float(1.0)])",
        ),
        ("neg step count", "len(range(3, 0, -1))\n", "Int(3)"),
        ("neg step empty", "len(range(0, 3, -1))\n", "Int(0)"),
    ]);
}

#[test]
fn d1_range_rejects_bad_args_instead_of_silently_defaulting() {
    // 恶劣失败模式的反面：类型不对要报错，不能悄悄换一个值。
    expect_all(&[
        (
            "step 0",
            "range(0, 5, 0)\n",
            "ERR: range() step must not be 0 (would never terminate)",
        ),
        (
            "non-numeric end",
            "range(0, \"x\")\n",
            "ERR: range() end must be a number, got string",
        ),
    ]);
}

// ─────────────────────────────────────────────────────────────────────
// D2：字符串长度必须是字符数
// ─────────────────────────────────────────────────────────────────────

#[test]
fn d2_string_len_counts_characters_not_bytes() {
    expect_all(&[
        ("ascii free", "len(\"abcd\")\n", "Int(4)"),
        ("ascii method", "\"abcd\".len()\n", "Int(4)"),
        // 修前：3 个汉字 = 9 字节 → 报 9
        ("cjk free", "len(\"中文字\")\n", "Int(3)"),
        ("cjk method", "\"中文字\".len()\n", "Int(3)"),
        ("mixed", "len(\"中文字abc\")\n", "Int(6)"),
        ("emoji", "len(\"a中b\")\n", "Int(3)"),
        ("empty", "len(\"\")\n", "Int(0)"),
    ]);
}

#[test]
fn d2_len_agrees_with_index_space() {
    // 核心不变式：`len(s)` 报告的下标空间必须与 `s[i]` 的合法下标空间一致。
    // 修前 `len("中文字") = 9` 但 `s[i]` 只接受 0..2，两者相差 3 倍。
    let s = "let s = \"中文字\"\n";
    let n = run(&format!("{s}len(s)\n"));
    assert_eq!(n, "Int(3)", "len 应为 3 个字符");

    // 最后一个字符必须可取，且恰好是 `len-1`
    let last = run(&format!("{s}s[len(s) - 1]\n"));
    assert_eq!(last, "Char('字')", "s[len(s)-1] 应取到最后一个字符");

    // 恰好在 len 处必须越界报错，且**报错里的长度也应是 3**（修前会是 9）
    let oob = run(&format!("{s}s[len(s)]\n"));
    assert_eq!(
        oob, "ERR: string index 3 out of bounds (len 3)",
        "越界报错的长度应与 len() 同口径"
    );
}

// ─────────────────────────────────────────────────────────────────────
// D3：字符串索引必须写得出
// ─────────────────────────────────────────────────────────────────────

#[test]
fn d3_string_index_works_from_source() {
    // 修前：字面量是 Float，字符串分支只匹配 Int → 任何 s[i] 都报
    // 「cannot index String(...) with Float(0.0)」。
    expect_all(&[
        ("s[0]", "let s=\"abc\"\ns[0]\n", "Char('a')"),
        ("s[1]", "let s=\"abc\"\ns[1]\n", "Char('b')"),
        ("s[2]", "let s=\"abc\"\ns[2]\n", "Char('c')"),
        (
            "s[3] oob",
            "let s=\"abc\"\ns[3]\n",
            "ERR: string index 3 out of bounds (len 3)",
        ),
        ("cjk s[0]", "let s=\"中文字\"\ns[0]\n", "Char('中')"),
        ("cjk s[2]", "let s=\"中文字\"\ns[2]\n", "Char('字')"),
    ]);
}

#[test]
fn d3_string_index_accepts_int_and_float_alike() {
    // List 索引本来就同时收 Int/Float，字符串现在与之对齐。
    // 两种下标写法必须取到同一个字符。
    let via_int = run("let s=\"abcd\"\nlet i = int(\"1\")\ns[i]\n");
    let via_float = run("let s=\"abcd\"\ns[1]\n");
    assert_eq!(via_int, "Char('b')");
    assert_eq!(via_float, via_int, "Int 下标与 Float 下标应取到同一字符");
}

#[test]
fn d3_iterating_a_string_yields_its_characters() {
    // 端到端：把 D2 的 len 与 D3 的索引合起来用。
    // 修前 `len` 是字节数，循环会在 i=3 处越界；更早时连 s[i] 都用不了。
    expect_all(&[
        (
            "concat all chars",
            "let s=\"中文字\"\nlet out=\"\"\nfor i in range(0, len(s))\n  assign out = out + str(s[i])\nend\nout\n",
            "String(\"中文字\")",
        ),
        (
            "reverse via index",
            "let s=\"abc\"\nlet out=\"\"\nfor i in range(0, len(s))\n  assign out = out + str(s[len(s) - 1 - i])\nend\nout\n",
            "String(\"cba\")",
        ),
    ]);
}

// ─────────────────────────────────────────────────────────────────────
// D4：int / float / bool
// ─────────────────────────────────────────────────────────────────────

#[test]
fn d4_conversion_builtins_exist_at_runtime() {
    // 修前三个全部 `ERR: Undefined function or task: …`
    expect_all(&[
        ("int(str)", "int(\"42\")\n", "Int(42)"),
        ("int(str pad)", "int(\" 42 \")\n", "Int(42)"),
        ("int(str negative)", "int(\"-7\")\n", "Int(-7)"),
        ("int(float) trunc", "int(4.7)\n", "Int(4)"),
        ("int(float) trunc neg", "int(-4.7)\n", "Int(-4)"),
        ("int(str with dot)", "int(\"42.9\")\n", "Int(42)"),
        ("float(str)", "float(\"1.5\")\n", "Float(1.5)"),
        ("float(int)", "float(2)\n", "Float(2.0)"),
        ("bool(zero)", "bool(0)\n", "Bool(false)"),
        ("bool(nonzero)", "bool(1)\n", "Bool(true)"),
        ("bool(empty str)", "bool(\"\")\n", "Bool(false)"),
        ("bool(nonempty str)", "bool(\"x\")\n", "Bool(true)"),
        ("bool(empty list)", "bool([])\n", "Bool(false)"),
        ("bool(nil)", "bool(nil)\n", "Bool(false)"),
    ]);
}

#[test]
fn d4_conversion_builtins_report_bad_input() {
    expect_all(&[
        (
            "int bad string",
            "int(\"abc\")\n",
            "ERR: int() cannot parse string: \"abc\"",
        ),
        (
            "int on list",
            "int([1,2])\n",
            "ERR: int() does not accept list",
        ),
        (
            "float bad string",
            "float(\"x\")\n",
            "ERR: float() cannot parse string: \"x\"",
        ),
        (
            "float on dict",
            "float({a:1})\n",
            "ERR: float() does not accept dict",
        ),
    ]);
}

#[test]
fn d4_conversions_round_trip_with_str() {
    // str / int / float 互相配合，验证三者确实在同一个运行期世界里。
    expect_all(&[
        ("str(int(str))", "str(int(\"42\"))\n", "String(\"42\")"),
        (
            "str(float(str))",
            "str(float(\"1.5\"))\n",
            "String(\"1.5\")",
        ),
        (
            "str(bool(int))",
            "str(bool(int(\"0\")))\n",
            "String(\"false\")",
        ),
    ]);
}

// ─────────────────────────────────────────────────────────────────────
// 跨族：等价写法一致性
// ─────────────────────────────────────────────────────────────────────

#[test]
fn all_len_spellings_agree_in_type_and_value() {
    // 五种 `len` 实现（list/dict/string 的自由函数与方法形式）此前
    // **返回不同的 Value 类型**：string 方法返 Float，其余四处返 Int。
    // 修后统一为 Int。
    let spellings = [
        ("list free", "len([1,2,3])\n"),
        ("list method", "[1,2,3].len()\n"),
        ("string free", "len(\"abc\")\n"),
        ("string method", "\"abc\".len()\n"),
        ("dict free", "len({a:1,b:2,c:3})\n"),
        ("dict method", "{a:1,b:2,c:3}.len()\n"),
    ];
    let mut seen = Vec::new();
    for (name, src) in spellings {
        let got = run(src);
        assert_eq!(got, "Int(3)", "[{name}] 期望 Int(3)，实得 {got}");
        seen.push(format!("{name}={got}"));
    }
    assert_eq!(seen.len(), 6, "六种写法都应被检查到");
}

#[test]
fn canonical_sum_spelling_is_stable() {
    // 「求和」这个最常见的循环形态，用 len() 写上界与用字面量写上界
    // 必须一致 —— 这是 D1 的行为级护栏。
    let by_len = run(
        "let xs=[1,2,3,4,5]\nlet s=0\nfor i in range(0, len(xs))\n  assign s = s + xs[i]\nend\ns\n",
    );
    let by_const =
        run("let xs=[1,2,3,4,5]\nlet s=0\nfor i in range(0, 5)\n  assign s = s + xs[i]\nend\ns\n");
    let by_while = run(
        "let xs=[1,2,3,4,5]\nlet s=0\nlet i=0\nwhile i < len(xs)\n  assign s = s + xs[i]\n  assign i = i + 1\nend\ns\n",
    );
    assert_eq!(by_len, "Float(15.0)");
    assert_eq!(by_len, by_const, "for+range 与字面量上界应一致");
    assert_eq!(by_len, by_while, "for+range 与 while+len 应一致");
}

// ─────────────────────────────────────────────────────────────────────
// 真实 CLI 路径
// ─────────────────────────────────────────────────────────────────────

/// 库内测试走 `run_mir`，**绕过 typeck**。而 `mora run` 会在
/// `main.rs:409` 跑 `check_program_witnesses_bidirectional`，出错即
/// `process::exit(2)`。D4 那一族（typeck 放行、运行期报 Undefined）
/// 只有在这条路径上才完整暴露，所以必须补一条子进程 e2e。
#[test]
fn cli_path_runs_all_four_fixes() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("tests/fixtures/e2e/builtin_gaps.mora")
        .output()
        .expect("run fixture");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "CLI 退出码 {:?}，应成功（typeck 放行 + 运行期无错）。\nstderr:\n{}",
        out.status.code(),
        stderr
    );

    // 逐行精确断言，避免「只要 stdout 里有 sum= 就通过」这种弱护栏。
    let expected = [
        "sum=15.0",   // D1：修前是 0.0
        "bound=5",    // D1：len 走 Int
        "chars=6",    // D2：修前是 12（UTF-8 字节）
        "method=6",   // D2：方法形式同样 6，且与自由函数同值
        "first=中",   // D3：修前 ERR: cannot index String(...) with Float(0.0)
        "last=c",     // D3：s[len(s)-1]
        "int=42",     // D4：修前 ERR: Undefined function or task: int
        "float=1.5",  // D4
        "bool=false", // D4
        "bool2=true", // D4
    ];
    for want in expected {
        assert!(
            stdout.contains(want),
            "CLI stdout 缺少 `{want}`。实际 stdout:\n{stdout}"
        );
    }
}

/// 修前这条 fixture 在 CLI 上根本跑不过 typeck 之后的运行期；
/// 现在必须**恰好**跑通，且不该冒出任何 Undefined function。
#[test]
fn cli_path_reports_no_undefined_builtin() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("tests/fixtures/e2e/builtin_gaps.mora")
        .output()
        .expect("run fixture");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for bad in [
        "Undefined function or task",
        "type error(s) found",
        "cannot index",
    ] {
        assert!(
            !combined.contains(bad),
            "CLI 输出里不该出现 `{bad}`，但有：\n{combined}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// typeck 的 `len` 返回类型：此前拒绝正确标注、接受错误标注
// ─────────────────────────────────────────────────────────────────────

/// 写一个临时 .mora 文件，经真实 CLI 跑，返回 `(exit_code, 合并输出)`。
fn cli_run_src(name: &str, src: &str) -> (i32, String) {
    use std::process::Command;
    let path = std::env::temp_dir().join(format!("mora_builtin_gaps_{name}.mora"));
    std::fs::write(&path, src).expect("write temp .mora");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg(&path)
        .output()
        .expect("run temp .mora");
    let _ = std::fs::remove_file(&path);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), combined)
}

/// `len` 的返回类型在 `typeck/hm/builtin.rs` 写 `Int`（对）、
/// 在 `typeck/dispatch.rs` 写 `Float`（错），而后者优先命中。
///
/// 结果是 typeck **拒绝正确标注、接受错误标注**，且被接受的那个标注
/// 与运行期实际拿到的值也不符：
///
/// ```text
/// 修前： let n: Int   = len(xs)  → exit 2「expected int, got float」
///        let n: Float = len(xs)  → exit 0，但运行期拿到的是 Int
/// ```
///
/// 这是全仓唯一一处「静态类型与动态值双向都不符」的地方，因此单独钉住。
#[test]
fn typeck_len_return_type_matches_runtime() {
    let (code_ok, out_ok) = cli_run_src(
        "len_int",
        "let xs = [1,2,3]\nlet n: Int = len(xs)\nprint(n)\n",
    );
    assert_eq!(
        code_ok, 0,
        "`let n: Int = len(xs)` 是**正确**标注，运行期返 Int，typeck 不该拒绝。\n{out_ok}"
    );
    assert!(
        out_ok.contains('\n') && out_ok.contains('3'),
        "应打印长度 3。\n{out_ok}"
    );

    let (code_bad, out_bad) = cli_run_src(
        "len_float",
        "let xs = [1,2,3]\nlet n: Float = len(xs)\nprint(n)\n",
    );
    assert_eq!(
        code_bad, 2,
        "`let n: Float = len(xs)` 与运行期的 Int 不符，typeck **应当**拒绝。\n{out_bad}"
    );
    assert!(
        out_bad.contains("Type error"),
        "拒绝理由应是类型不匹配。\n{out_bad}"
    );
}

/// 方法形式 `.len()` 的三处签名此前同样声明 `Float`，一并钉住。
#[test]
fn typeck_len_method_return_type_matches_runtime() {
    for (name, src) in [
        (
            "list",
            "let xs = [1,2,3]\nlet n: Int = xs.len()\nprint(n)\n",
        ),
        (
            "dict",
            "let d = {a:1,b:2}\nlet n: Int = d.len()\nprint(n)\n",
        ),
        (
            "string",
            "let s = \"abc\"\nlet n: Int = s.len()\nprint(n)\n",
        ),
    ] {
        let (code, out) = cli_run_src(&format!("len_m_{name}"), src);
        assert_eq!(
            code, 0,
            "`{name}.len()` 返回 Int，`let n: Int` 应通过。\n{out}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// D6：模块前缀当自由函数调 → panic
// ─────────────────────────────────────────────────────────────────────

/// `call_function` 的 `_` 兜底臂上有一道 `testcase!` 守卫，它断言
/// 「凡是 `BuiltinKind::from_name` 登记过的名字，都必须有 `match name` 分支」。
///
/// 但 `from_name` 有**两个**职责：方法前缀查找 与 自由函数可调用性。
/// `MODULE_OBJECTS`（`math` / `stats` / `linalg` / `json` / `file` / `web` /
/// `random` / … 共 20 个）是为**前者**登记的 —— `math.floor(x)` 走
/// `call_method_*`，从不经过 `call_function`。它们**理应**没有自由函数
/// 分支、**理应**落兜底。
///
/// 守卫只看 `from_name` 的返回值，于是把「模块前缀当函数调」判成登记漂移：
///
/// ```text
/// math(2.5)   → *** PANIC ***     修后：ERR: 'math' is a module, not a function
/// json(1)     → *** PANIC ***     修后：ERR: 'json' is a module, not a function
/// ```
///
/// 20 个前缀**全部**中招，且 typeck 不拦（`math`/`stats`/`linalg` 本就登记了
/// marker signature）。兜底本来会给一句完全正确的 `'math' is not callable`
/// —— 是守卫把它变成了崩溃。
///
/// 判据用 `catch_unwind` 而不是「期望拿到 Err 字符串」：一个会 panic 的
/// 实现也能让「断言里有错误文本」通过，只有真的没崩才算数。
#[test]
fn module_prefix_called_bare_does_not_panic() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    // 全部 20 个 MODULE_OBJECTS 前缀，逐个验证「不崩 + 给出指路错误」
    let prefixes = [
        "ai", "web", "json", "file", "memory", "agent", "document", "bus", "sandbox", "schedule",
        "ccr", "mock", "exec", "tool", "skill", "plan", "mora", "math", "stats", "linalg",
        "random",
    ];
    let mut failures = Vec::new();
    for p in prefixes {
        let src = format!("{p}(1)\n");
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = catch_unwind(AssertUnwindSafe(|| run(&src)));
        std::panic::set_hook(prev);
        let got = match r {
            Ok(s) => s,
            Err(_) => {
                failures.push(format!("  [{p}] PANIC —— 模块前缀当自由函数调会崩溃"));
                continue;
            }
        };
        if !got.contains("is a module, not a function") {
            failures.push(format!("  [{p}] 期望指路错误，实得 {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个模块前缀的裸调用行为不对：\n{}",
        failures.len(),
        prefixes.len(),
        failures.join("\n")
    );
}

/// 修前这道守卫的前提就是错的（把「登记为方法前缀」当成「可作自由函数调用」）。
/// 合法写法必须一条都不受影响。
#[test]
fn module_method_calls_still_work() {
    expect_all(&[
        ("math.floor", "math.floor(2.5)\n", "Float(2.0)"),
        ("math.abs", "math.abs(-3)\n", "Float(3.0)"),
        ("stats.mean", "stats.mean([1,2,3])\n", "Float(2.0)"),
        ("linalg.norm", "linalg.norm([3,4])\n", "Float(5.0)"),
        ("json.parse", "json.parse(\"{}\")\n", "Dict({})"),
    ]);
}

/// 用户可以用同名绑定遮蔽模块名 —— 此时 `math(x)` 仍应是**普通函数调用**，
/// 不能被新增的「这是模块」分支误伤。
#[test]
fn shadowed_module_name_is_still_callable() {
    expect_all(&[(
        "let math = fn(x) x * 2 end",
        "let math = fn(x) x * 2 end\nmath(21)\n",
        "Float(42.0)",
    )]);
}

// ─────────────────────────────────────────────────────────────────────
// 方法侧三方对拍：typeck 注册 / 运行期实现 / methods_of 报告
// ─────────────────────────────────────────────────────────────────────

/// `methods_of(x)` 是用户发现「这个值能干什么」的唯一入口。
/// 它若**多报**未实现的方法，用户会撞上「报了却调不通」；若**漏报**已实现的
/// 方法，用户会以为能力不存在。两种都是这张表在骗人。
///
/// v0.104.6 抓到 `List.crush_json` 漏报：`call_method_list` 有该分支（实测
/// 报 `List.crush_json: requires max as number`，证明分支存在），但
/// `Value::methods()` 的 List 名单没列它。
/// 一条 `methods_of` 用例：`(值类型, 值字面量, [(方法名, 可调用的源码)])`。
type MethodCase = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
);

#[test]
fn methods_of_never_reports_an_unimplemented_method() {
    // (类型, 值字面量) —— 逐个调 methods_of 报的每个名字，验证都能真调
    let cases: Vec<MethodCase> = vec![
        (
            "list",
            "[3,1,2]",
            vec![
                ("len", "[3,1,2].len()"),
                ("get", "[3,1,2].get(0)"),
                ("sort", "[3,1,2].sort()"),
                ("sum", "[3,1,2].sum()"),
                ("min", "[3,1,2].min()"),
                ("max", "[3,1,2].max()"),
                ("mean", "[3,1,2].mean()"),
                ("median", "[3,1,2].median()"),
                ("var", "[3,1,2].var()"),
                ("stddev", "[3,1,2].stddev()"),
                ("shape", "[3,1,2].shape()"),
                ("flatten", "[3,1,2].flatten()"),
                ("pop", "[3,1,2].pop()"),
                ("push", "[3,1,2].push(9)"),
                ("map", "[3,1,2].map(fn(x) x end)"),
                ("filter", "[3,1,2].filter(fn(x) x > 1 end)"),
                ("reduce", "[3,1,2].reduce(fn(a, b) a + b end, 0)"),
                ("take", "[3,1,2].take(2)"),
                ("drop", "[3,1,2].drop(1)"),
                ("window", "[3,1,2].window(2)"),
                ("batch", "[3,1,2].batch(2)"),
                ("reshape", "[3,1,2].reshape(1, 3)"),
                ("transpose", "[[1,2],[3,4]].transpose()"),
                ("crush_json", "[3,1,2].crush_json(2)"),
            ],
        ),
        (
            "dict",
            "{a:1,b:2}",
            vec![
                ("get", "{a:1,b:2}.get(\"a\")"),
                ("keys", "{a:1,b:2}.keys()"),
                ("values", "{a:1,b:2}.values()"),
                ("len", "{a:1,b:2}.len()"),
                ("json", "{a:1,b:2}.json()"),
            ],
        ),
        (
            "string",
            "\"abc\"",
            vec![
                ("len", "\"abc\".len()"),
                ("upper", "\"abc\".upper()"),
                ("lower", "\"ABC\".lower()"),
                ("trim", "\" a \".trim()"),
                ("starts_with", "\"abc\".starts_with(\"a\")"),
                ("ends_with", "\"abc\".ends_with(\"c\")"),
                ("contains", "\"abc\".contains(\"b\")"),
                ("split", "\"a,b\".split(\",\")"),
                ("replace", "\"abc\".replace(\"a\", \"z\")"),
                ("json", "\"abc\".json()"),
            ],
        ),
    ];

    let mut failures = Vec::new();
    for (kind, literal, probes) in cases {
        // 先问 methods_of 有哪些方法
        let reported = run(&format!("methods_of({literal})\n"));
        for (method, call) in probes {
            if !reported.contains(&format!("\"{method}\"")) {
                failures.push(format!(
                    "  [{kind}] methods_of 未报 `{method}`，但运行期实现了（{call}）"
                ));
                continue;
            }
            // 报了，就必须真能调（下面 probe 已给足实参）
            let got = run_runtime_only(&format!("{call}\n"));
            if got.starts_with("ERR:") || got.starts_with("COMPILE-ERR:") {
                failures.push(format!(
                    "  [{kind}] methods_of 报了 `{method}`，实调却失败：{call} → {got}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "methods_of 与运行期不一致：\n{}",
        failures.join("\n")
    );
}

/// D7：`Type::AiConfig` / `Type::HttpRequest` 的方法签名是**不可达死注册**。
///
/// `Value::AiConfig` 与 `Value::HttpRequest` 全仓从未被构造，`call_method`
/// 也没有它们的分支。所以今天这不是用户可见缺陷 —— 但它是一枚**地雷**：
/// 谁哪天补上构造入口，这些签名立刻变成「typeck 放行 → 运行期报
/// Can only call methods on …」。
///
/// 这里钉住「确实拿不到这样的值」这个事实，好让补构造的人看见这条测试
/// 失败并知道还要补 `call_method` 分支。
#[test]
fn ai_config_and_http_request_values_are_unconstructible() {
    // 构造入口不存在
    expect_all(&[(
        "AiConfig::new()",
        "AiConfig::new()\n",
        "ERR: Undefined function or task: AiConfig::new",
    )]);
    // `with` 块把配置存进 Rust 侧的 CoreRuntime，不是 Value::AiConfig
    assert_eq!(
        run("with model = \"m\"\n  1\nend\n"),
        "Nil",
        "`with` 块不产生 AiConfig 值"
    );
}

/// 反向对拍：typeck 为 String/Dict/List 注册的每个方法名，运行期都得有分支。
/// 这一侧是「typeck 放行 → 运行期 Unknown method」那一族（v0.104.6 修掉的
/// `str()` / `int()` / `float()` / `bool()` 在自由函数侧；方法侧此前干净，
/// 由本测试保持干净）。
#[test]
fn typeck_registered_methods_all_exist_at_runtime() {
    let probes: &[(&str, &str)] = &[
        // typeck 的 (String, m)
        ("len", "\"ab\".len()"),
        ("upper", "\"ab\".upper()"),
        ("lower", "\"ab\".lower()"),
        ("trim", "\" a \".trim()"),
        ("replace", "\"ab\".replace(\"a\", \"b\")"),
        ("starts_with", "\"ab\".starts_with(\"a\")"),
        ("ends_with", "\"ab\".ends_with(\"b\")"),
        ("contains", "\"ab\".contains(\"a\")"),
        ("split", "\"a,b\".split(\",\")"),
        // typeck 的 (Dict, m)
        ("get", "{a:1}.get(\"a\")"),
        ("set", "{a:1}.set(\"b\", 2)"),
        ("keys", "{a:1}.keys()"),
        ("values", "{a:1}.values()"),
        // typeck 的 (List, m)
        ("map", "[1].map(fn(x) x end)"),
        ("filter", "[1].filter(fn(x) true end)"),
        ("push", "[1].push(2)"),
        ("pop", "[1].pop()"),
    ];
    let mut failures = Vec::new();
    for (method, call) in probes {
        let got = run_runtime_only(&format!("{call}\n"));
        if got.starts_with("ERR:") || got.starts_with("COMPILE-ERR:") {
            failures.push(format!("  [{method}] {call} → {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "typeck 注册了但运行期调不通的方法：\n{}",
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// D9：ai.chat 的返回类型 —— 语言主打能力从源码层完全不可达
// ─────────────────────────────────────────────────────────────────────

/// typeck 把 `ai.chat` 声明为 `-> Type::AiResult`，而 `Value` 里**没有任何**
/// `AiResult` 变体 —— 它是个只存在于 typeck 的幽灵类型。于是返回值没有任何
/// 东西能消费，连 `print` 的形参类型里都没有它：
///
/// ```text
/// 修前（真实 CLI `mora run`，均 exit 2）：
///   let reply = ai.chat("hi")     Type mismatch: expected string|int|…, got ai_result
///   print(ai.chat("hi"))          同上
/// ```
///
/// 连「先赋值再打印」都过不了 —— **`ai.chat` 在源语言里任何用法都用不了**。
/// 而 `do_ai_chat` 的每一条路径（mock / replay / cache / real / tools /
/// agent）都返回 `Value::String`；运行期自己的记录签名也写着
/// `"ai.chat(model: string, prompt: string) -> string"`。是 typeck 写错了。
///
/// 这里逐条钉住常见用法的**可编译性** —— 判据是「typeck 放行」，所以必须走
/// 真实 CLI（库内 `run_mir` 绕过 typeck，见本文件上方说明）。
#[test]
fn ai_chat_is_usable_from_source() {
    let cases: &[(&str, &str)] = &[
        ("直接 print", "print(ai.chat(\"hi\"))\n"),
        ("赋值再 print", "let r = ai.chat(\"hi\")\nprint(r)\n"),
        (
            "字符串拼接",
            "let r = ai.chat(\"hi\")\nprint(\"reply: \" + r)\n",
        ),
        ("标注 String", "let r: String = ai.chat(\"hi\")\nprint(r)\n"),
        ("当作值返回", "ai.chat(\"hi\")\n"),
    ];
    let mut failures = Vec::new();
    for (i, (name, src)) in cases.iter().enumerate() {
        let (code, out) = cli_run_src(&format!("chat_{i}"), src);
        if code != 0 {
            failures.push(format!("  [{name}] exit={code}\n{out}"));
        }
    }
    assert!(
        failures.is_empty(),
        "ai.chat 在源语言里不可用：\n{}",
        failures.join("\n")
    );
}

/// 顺带把整个 AI surface 扫一遍 —— 同一族的阻塞可能不止 `ai.chat` 一处。
#[test]
fn ai_surface_is_usable_from_source() {
    let cases: &[(&str, &str)] = &[
        ("p\"...\" 赋值", "let r = p\"say hi\"\nprint(r)\n"),
        ("p\"...\" 直接 print", "print(p\"say hi\")\n"),
        ("ai.critic", "let v = ai.critic(\"answer\")\nprint(v)\n"),
        (
            "ai.critic 取 verdict",
            "let v = ai.critic(\"answer\")\nprint(v.get(\"verdict\"))\n",
        ),
        ("ai.tokens.input", "print(ai.tokens.input)\n"),
        ("ai.tokens.total", "print(ai.tokens.total)\n"),
        (
            "with 块内 chat",
            "with model = \"m\"\n  print(ai.chat(\"hi\"))\nend\n",
        ),
        ("Router::new", "let r = Router::new()\nprint(type_of(r))\n"),
        (
            "McpServer::new",
            "let m = McpServer::new()\nprint(type_of(m))\n",
        ),
    ];
    let mut failures = Vec::new();
    for (i, (name, src)) in cases.iter().enumerate() {
        let (code, out) = cli_run_src(&format!("surface_{i}"), src);
        if code != 0 {
            failures.push(format!("  [{name}] exit={code}\n{out}"));
        }
    }
    assert!(
        failures.is_empty(),
        "AI surface 有写法过不了类型检查：\n{}",
        failures.join("\n")
    );
}

/// `ai.critic` 不只该能编译，还得真给出 spec §12.5 承诺的结构化裁决。
#[test]
fn ai_critic_returns_structured_verdict() {
    let (code, out) = cli_run_src("critic_shape", "print(ai.critic(\"the answer\"))\n");
    assert_eq!(code, 0, "ai.critic 应能跑通。\n{out}");
    for key in ["verdict", "critique", "score"] {
        assert!(
            out.contains(key),
            "裁决 dict 应含 `{key}` 字段（spec §12.5）。\n{out}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// D14：import 进来的模块里 handle + 索引
// ─────────────────────────────────────────────────────────────────────

/// `import` 编译被导入的模块时走**裸 `ParserV3::compile` 的 witness 路径**
/// （不过 9 层管线），而 witness 把读索引编码成 `Call("[]")`、lower 此前
/// 未解码 → `Undefined function or task: []`。
///
/// 判据必须走**真实 CLI** —— Rust 侧只能模拟裸路径，`import` 这个生产
/// 入口只有 CLI 才真正经过（`mir_import` → `ParserV3::compile`）。
/// 详细根因见 `tests/pipeline_equivalence.rs` 的文件头。
#[test]
fn imported_module_handle_index_works() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("tests/fixtures/e2e/import_handle_index_main.mora")
        .output()
        .expect("run import fixture");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "import 进来的 handle+索引应能跑通。\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("20.0"),
        "t = [10,20,30]，t[1] 应得 20.0。实际 stdout:\n{stdout}"
    );
    assert!(
        !format!("{stdout}{stderr}").contains("Undefined function or task: []"),
        "不该再把索引当成调用一个名叫 `[]` 的函数"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 幽灵类型：typeck 有、`Value` 没有 —— 保持它们不可达
// ─────────────────────────────────────────────────────────────────────

/// `Type` 有 28 个变体，`Value` 有 25 个。扣掉设计使然的三类之后，
/// 真正「只存在于 typeck」的变体是：
///
/// | Type 变体 | 状态 |
/// |-----------|------|
/// | `AiModule` | ✅ 映射到 `Value::Builtin(BuiltinKind::AiChat)` |
/// | `RandomModule` | ✅ 映射到 `Value::Builtin(BuiltinKind::Random)` |
/// | `Any` / `Unknown` / `Union` | ✅ 顶类型 / 逃生口 / 类型层构造 |
/// | `Tuple` / `HttpResponse` / `AiError` / `Task` | 死变体，parser 已挡 |
/// | `AiConfig` / `HttpRequest` | 变体存在但**全仓从不构造** |
/// | `AiResult` | ✅ v0.104.6 已从 `ai.chat` 返回类型移除 |
///
/// **幽灵类型本身无害** —— 它们既不能被构造出值，也写不成类型标注
/// （`parser_v3/syntax.rs:738` 的标注白名单只有 7 个名字，其余报
/// `unsupported type annotation`）。真正危险的是**幽灵类型被挂进一个可达
/// production 的签名**：`AiResult` 当年正是 `ai.chat` 的返回类型，于是
/// `ai.chat` 的返回值谁也消费不了，整个主打能力从源码层不可达（D9）。
///
/// 本测试钉住「其余幽灵类型写不成标注」这个不变式 —— 它是它们无害的原因。
/// 哪天有人往白名单里加一个幽灵类型，这里会失败，并提示同时要给 `Value`
/// 加对应变体。
#[test]
fn phantom_types_are_not_writable_as_annotations() {
    let phantom = [
        "Tuple",
        "HttpResponse",
        "AiError",
        "AiConfig",
        "AiResult",
        "Task",
        "Conversation",
        "Router",
        "McpServer",
        // ⚠ v0.104.6 D85：`Agent` 已从本名单**移出** —— 它**不是**幽灵类型。
        // `Value::Agent` 存在，且 `agent.create(name, cfg)` 真实产出它
        // （`method_dispatch.rs` 的 `(BuiltinKind::Agent, "create")` arm）。
        // D85 给它补了 typeck 签名后，parser 白名单也加了 `agent`，
        // 于是 `let v: Agent = …` 从「解析期被挡」变成「类型检查期被拒」——
        // 拒绝仍在，只是换了层。留在名单里会让本测试报
        // 「被拒，但不是白名单所拒」。
        //
        // 与 `Router` / `McpServer` 的区别：那两个**没有**同名的小写标注
        // （`Router::new()` 是构造器，不是标注名），故仍属不可写。
    ];
    let mut failures = Vec::new();
    for t in phantom {
        let (code, out) = cli_run_src(
            &format!("phantom_{t}"),
            &format!("let v: {t} = 1\nprint(v)\n"),
        );
        // 期望被 parser 挡下（`unsupported type annotation`），而不是跑通
        if code == 0 {
            failures.push(format!(
                "  [{t}] 幽灵类型竟能写成类型标注并通过检查 —— 若要放开，必须同时给 \
                 `Value` 加对应变体并给 `call_method` 补分支"
            ));
        } else if !out.contains("unsupported type annotation") {
            failures.push(format!("  [{t}] 被拒，但不是白名单所拒：\n{out}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个幽灵类型不再被 parser 挡住：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 对照组：白名单内的标注**必须**真的在检查（否则「挡住幽灵类型」只是
/// 因为所有标注都被忽略，那是更糟的另一种失效）。
#[test]
fn writable_annotations_are_actually_checked() {
    // 正确的标注通过
    let (code_ok, out_ok) = cli_run_src("annot_ok", "let s: String = \"x\"\nprint(s)\n");
    assert_eq!(code_ok, 0, "正确标注应通过。\n{out_ok}");
    // 错误的标注被拒
    let (code_bad, out_bad) = cli_run_src("annot_bad", "let s: String = 1\nprint(s)\n");
    assert_eq!(code_bad, 2, "错误标注应被 typeck 拒绝。\n{out_bad}");
    assert!(
        out_bad.contains("Type error"),
        "拒绝理由应是类型不匹配。\n{out_bad}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 数值方法：methods() 覆盖 Float/Int/BigInt 共 45 个名字
// ─────────────────────────────────────────────────────────────────────

/// `Value::methods()` 的三张数值表此前从未被审计过 —— `methods_of` 对拍只
/// 覆盖了 List / Dict / String，而这 45 个名字才是最大的三张表。
///
/// 逐个实调，全部通过：**Float 27 + Int 14 + BigInt 4，无一虚报**。
#[test]
fn numeric_methods_of_are_all_callable() {
    let float_methods = [
        "abs",
        "sign",
        "floor",
        "ceil",
        "round",
        "trunc",
        "fract",
        "sqrt",
        "cbrt",
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "sinh",
        "cosh",
        "tanh",
        "exp",
        "log",
        "log2",
        "log10",
        "log1p",
        "is_nan",
        "is_inf",
        "is_finite",
        "to_int",
    ];
    let int_methods = [
        "abs", "sign", "floor", "ceil", "round", "sqrt", "sin", "cos", "tan", "exp", "log", "log2",
        "log10", "to_float",
    ];
    let bigint_methods = ["abs", "sign", "to_float", "to_int"];

    let cases: Vec<(&str, &str, &[&str])> = vec![
        // 注意 receiver 必须是**真 Float/Int**，不能写字面量 3 —— 本语言
        // 数值字面量都是 Float（`type_of(3)` = "float"），`methods_of(3)`
        // 报的是 Float 表而不是 Int 表。
        ("Float", "1.5", &float_methods),
        ("Int", "int(\"3\")", &int_methods),
        ("BigInt", "5n", &bigint_methods),
    ];

    let mut failures = Vec::new();
    for (kind, recv, methods) in cases {
        let reported = run(&format!("methods_of({recv})\n"));
        for m in methods {
            if !reported.contains(&format!("\"{m}\"")) {
                failures.push(format!("  [{kind}] methods_of 未报 `{m}`"));
                continue;
            }
            let got = run(&format!("{recv}.{m}()\n"));
            if got.starts_with("ERR:") || got.starts_with("COMPILE-ERR:") {
                failures.push(format!("  [{kind}] methods_of 报了 `{m}`，实调失败：{got}"));
            }
        }
        // 反向：methods_of 报了但上面清单里没有的名字，也要能调
        for name in reported.split('"').skip(1).step_by(2).collect::<Vec<_>>() {
            if methods.contains(&name) {
                continue;
            }
            let got = run(&format!("{recv}.{name}()\n"));
            if got.starts_with("ERR:") || got.starts_with("COMPILE-ERR:") {
                failures.push(format!(
                    "  [{kind}] methods_of 报了 `{name}`，实调失败：{got}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "数值方法的 methods() 与运行期不一致：\n{}",
        failures.join("\n")
    );
}

/// `numeric_helpers.rs` 的注释一直写着「`math.abs(x)` / `x.abs()` 走同一
/// 底层函数，结果一致」—— 但那是**未经实测的断言**。这里把它变成护栏。
///
/// 覆盖负数，因为 `floor/ceil/round/trunc/fract` 在负半轴上最容易分歧；
/// `sqrt` 的负数入参两边都应返 `NaN` 而非报错。
#[test]
fn numeric_method_and_math_module_agree() {
    let methods = [
        "abs", "sign", "floor", "ceil", "round", "trunc", "fract", "sqrt", "cbrt", "sin", "cos",
        "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "log", "log2", "log10",
        "log1p",
    ];
    let mut failures = Vec::new();
    for m in methods {
        for arg in ["2.5", "-2.5", "0.5", "8.0"] {
            // 负数**必须加括号**：`-2.5.abs()` 解析为 `-(2.5.abs())`（标准
            // 数学优先级，类比 `-x^2`），那不是两种写法不一致。
            let via_method = run(&format!("({arg}).{m}()\n"));
            let via_module = run(&format!("math.{m}({arg})\n"));
            if via_method != via_module {
                failures.push(format!(
                    "  [{m} @ {arg}] 方法式={via_method}  模块式={via_module}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} 处 `x.m()` 与 `math.m(x)` 结果发散：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 把「负数加不加括号」这个易误判点钉成显式事实，免得后来人把它当缺陷
/// 报一遍（`abs` 那一处我第一轮就误判了）。
#[test]
fn method_call_binds_tighter_than_unary_minus() {
    // `-2.5.abs()` = `-(2.5.abs())` = -2.5
    expect_all(&[
        ("unary minus applies last", "-2.5.abs()\n", "Float(-2.5)"),
        ("parenthesized", "(-2.5).abs()\n", "Float(2.5)"),
        ("module form", "math.abs(-2.5)\n", "Float(2.5)"),
        // `0 - 2.5.abs()` 同得 -2.5，证明是「方法先算、再相减」
        ("binary minus", "0 - 2.5.abs()\n", "Float(-2.5)"),
    ]);
}

/// `methods_of` 必须按**运行期真实类型**分表。本语言数值字面量都是 Float，
/// 所以 `methods_of(3)` 报 Float 表是对的；只有真 Int 才报 Int 表。
/// 这条很容易在写测试时误判（我第一轮就踩了），故显式钉住。
#[test]
fn methods_of_routes_by_runtime_type_not_literal_shape() {
    // 字面量 3 是 Float
    assert_eq!(run("type_of(3)\n"), "String(\"float\")");
    let float_tbl = run("methods_of(3)\n");
    assert!(
        float_tbl.contains("\"trunc\"") && float_tbl.contains("\"to_int\""),
        "Float 表应含 trunc/to_int：{float_tbl}"
    );
    // 真 Int
    assert_eq!(run("type_of(int(\"3\"))\n"), "String(\"int\")");
    for src in ["methods_of(int(\"3\"))", "methods_of(len([1,2,3]))"] {
        let tbl = run(&format!("{src}\n"));
        assert!(
            tbl.contains("\"to_float\"") && !tbl.contains("\"trunc\""),
            "{src} 应报 Int 表（含 to_float、不含 trunc）：{tbl}"
        );
    }
}

/// `bool(x)` 必须与语言的真值判断**逐值一致**。
///
/// `flow::is_truthy` 自称「MIR 条件分支的单一真值源（v0.75.83 收敛）」，
/// 并明确警告「两处语义分叉是隐蔽 bug 温床」。v0.104.6 给 `bool()` builtin
/// 写实现时，我在那里**手搓了一张真值表**，正好踩进它警告的坑 ——
/// 实测 4 处分叉：
///
/// ```text
/// 0n (BigInt)      if 判真 = true    bool() = false   ← 判反了
/// fn(x) x end      if 判真 = true    bool() 报错
/// Router::new()    if 判真 = true    bool() 报错
/// McpServer::new() if 判真 = true    bool() 报错
/// ```
///
/// 根因：`is_truthy` 的兜底臂是 `_ => true`（未知类型一律为真），而手搓的表
/// 对未知类型报错、又给 BigInt 另判了一套。修法是委托，分叉从根上消失。
///
/// 判据用 `if <expr> … end` 与 `bool(<expr>)` 对照 —— 前者走语言的分支真值
/// 判断，后者走 builtin，两者必须同进同出。
#[test]
fn bool_agrees_with_language_truthiness() {
    let exprs: &[(&str, &str)] = &[
        ("nil", "nil"),
        ("true", "true"),
        ("false", "false"),
        ("int 0", "0"),
        ("float 0.0", "0.0"),
        ("bigint 0n", "0n"),
        ("bigint 1n", "1n"),
        ("empty string", "\"\""),
        ("nonempty string", "\"a\""),
        ("empty list", "[]"),
        ("nonempty list", "[1]"),
        ("empty dict", "{}"),
        ("nonempty dict", "{a: 1}"),
        ("closure", "fn(x) x end"),
        ("Router", "Router::new()"),
        ("McpServer", "McpServer::new()"),
        ("agent", "agent.create(\"x\", {})"),
    ];
    let mut failures = Vec::new();
    for (name, e) in exprs {
        // if 走语言真值判断：真分支返 1.0，假分支返 0.0
        let t = run_runtime_only(&format!("if {e}\n  1\nelse\n  0\nend\n"));
        let truthy = match t.as_str() {
            "Float(1.0)" => "true",
            "Float(0.0)" => "false",
            other => {
                failures.push(format!("  [{name}] if 判真返回了意外值 {other}"));
                continue;
            }
        };
        let b = run_runtime_only(&format!("bool({e})\n"));
        let as_bool = b
            .strip_prefix("Bool(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(&b);
        if as_bool != truthy {
            failures.push(format!(
                "  [{name}] if 判真 = {truthy}，但 bool() = {as_bool}（分叉！）"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "bool() 与语言真值判断分叉 —— 应直接委托 `flow::is_truthy`：\n{}",
        failures.join("\n")
    );
}

/// ambient 兜底失败时必须报**真实原因**，不能一律说「运行期不变量被破坏」。
///
/// `MirHost::perform_effect` 的签名是 `Option<Value>`，没有错误通道，
/// `random_state.dispatch_op(...).ok()` 把 `Err` 吞成 `None`。于是哪怕
/// handler **跑到了**、只是实参校验没过，调用方也只看到：
///
/// ```text
/// unhandled effect: random_rand_float
///   (ambient random state missing — runtime invariant violated)
/// ```
///
/// 「runtime invariant violated」把诊断指向不存在的基建故障，实际是自己的
/// 调用写错了。修法：`perform_effect` 把原始消息暂存到
/// `CoreRuntime::last_ambient_error`，`call_random_ambient` 取走还原。
///
/// 走 `mora run` 时 typeck 的 ambient 签名预置会先拦（arity 与实参类型都查），
/// 所以这条只对**库内路径**可见 —— 但库内路径正是全部测试套件与任何嵌入
/// Mora 的调用方走的路。本测试走的正是库内路径。
#[test]
fn ambient_failure_reports_real_reason_not_invariant_violation() {
    let got = run_runtime_only("random.rand_float()\n");
    assert!(
        got.contains("requires (min, max)"),
        "应报 dispatch_op 的真实参数错误，实得：{got}"
    );
    assert!(
        !got.contains("runtime invariant violated"),
        "不应把实参错误误报成运行期不变量被破坏：{got}"
    );
    assert!(
        !got.contains("unhandled effect"),
        "handler 跑到了，不该说 unhandled：{got}"
    );
}

/// 对照：标签本身不认识时，才是真正的 unhandled，且那条信息仍要保留。
#[test]
fn unknown_random_method_is_still_reported_as_unknown_method() {
    let got = run_runtime_only("random.bogus()\n");
    assert!(
        got.contains("unknown method"),
        "未知方法应报 unknown method，实得：{got}"
    );
}

/// 正常路径不受影响：暂存的错误消息不能串到下一次成功调用上。
#[test]
fn ambient_state_recovers_after_a_failed_call() {
    // 先失败一次（写入 last_ambient_error），再成功调用一次
    let bad = run_runtime_only("random.rand_float()\n");
    assert!(bad.contains("requires (min, max)"), "{bad}");
    // 注意末行要是**调用本身**：`let a = …` 是语句，值是 Nil
    let good = run_runtime_only("random.rand_float(0, 1)\n");
    assert!(
        good.starts_with("Float(") && !good.starts_with("ERR"),
        "失败后仍应能正常调用，实得：{good}"
    );
    // 再失败一次，错误信息仍是本次的真实原因（不是陈旧的）
    let bad2 = run_runtime_only("random.rand_int()\n");
    assert!(
        bad2.contains("requires (min, max)"),
        "第二次失败应报本方法的真实原因（rand_int 而非残留的 rand_float），实得：{bad2}"
    );
}

/// `ambient` 模块的标签表与方法映射必须一致 —— 两张表在同一模块内平行维护，
/// 漂了就会「标签在权威名单里但方法映射找不到」，或反之。
#[test]
fn ambient_labels_and_method_mapping_agree() {
    use mora::mir::effect::ambient;
    let mut failures = Vec::new();
    for label in ambient::RANDOM_LABELS {
        let Some(method) = label.strip_prefix("random_") else {
            failures.push(format!("  [{label}] 不符合命名规则 `random_` + 方法名"));
            continue;
        };
        if ambient::random_label_for_method(method) != Some(label) {
            failures.push(format!(
                "  [{label}] `random_label_for_method(\"{method}\")` 未映回同一标签"
            ));
        }
        if !ambient::is_ambient_label(label) {
            failures.push(format!("  [{label}] is_ambient_label 说不它是 ambient"));
        }
    }
    assert!(
        failures.is_empty(),
        "ambient 标签表与方法映射漂移：\n{}",
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// D10：McpServer.tool 丢弃 schema —— 客户端看到「工具无参数」
// ─────────────────────────────────────────────────────────────────────

/// `McpServer.tool(name, schema, handler)` 读 `args[0]` 与 `args[2]`，
/// **把 `args[1]`（schema）整个丢掉** —— `tools` 当时是
/// `Vec<(String, Value)>`，结构上就存不下；`serve` 再把
/// `McpTool.parameters` 硬编码成 `"{}"`。
///
/// 后果在 MCP 协议里：`mcp_server.rs` 的 `tools/list` 把 `parameters` 作为
/// **`inputSchema`** 发给客户端，于是**每个工具都宣称「无参数」**，客户端
/// 对需要入参的 handler 以空参调用。typeck 一直声明的是三形参
/// `tool(name, schema, handler)` —— 契约对，运行期没兑现。
///
/// 内部三元组的断言在 `src/interpreter/dispatch.rs::tests`（源码侧看不到
/// `McpServer` 的内部结构，它没有 `json` 方法）。这里钉源码可观测的那一面。
#[test]
fn mcp_tool_rejects_non_schema_argument() {
    // 非 dict / 非 JSON 字符串的 schema 应**报错**，不能静默降级成 `{}`
    let (code, out) = cli_run_src(
        "mcp_bad_schema",
        "let m = McpServer::new()\nm.tool(\"t\", 1, fn(x) x end)\n",
    );
    assert_ne!(
        code, 0,
        "数字 schema 应被拒（静默变空对象会让客户端以为无参数）"
    );
    assert!(
        out.contains("schema must be a dict or JSON string"),
        "错误信息应点明 schema 的合法形态。\n{out}"
    );
}

/// `tool()` 与 `push` / `sort` 同语义：**返回新值，接收者不变**。
/// 这条很容易写错 —— 不接返回值会看起来像「工具没注册成功」而误判成缺陷。
#[test]
fn mcp_tool_returns_new_server_and_leaves_receiver_unchanged() {
    // 显示文本只能从 stdout 拿（`print` 自身返回 Nil），故走 CLI。
    // 库内 `run()` 返回**最后一个表达式的值**，不是 stdout。
    let cases: &[(&str, &str, &str)] = &[
        (
            "原接收者不变",
            "let m = McpServer::new()\nlet m2 = m.tool(\"t\", {a: 1}, fn(x) x end)\nprint(m)\n",
            "<mcp_server (0 tools)>",
        ),
        (
            "返回值含 1 个工具",
            "let m = McpServer::new()\nlet m2 = m.tool(\"t\", {a: 1}, fn(x) x end)\nprint(m2)\n",
            "<mcp_server (1 tools)>",
        ),
    ];
    for (i, (name, src, want)) in cases.iter().enumerate() {
        let (code, out) = cli_run_src(&format!("mcp_ret_{i}"), src);
        assert_eq!(code, 0, "[{name}] 应跑通。\n{out}");
        assert!(
            out.contains(want),
            "[{name}] 期望 stdout 含 `{want}`。\n{out}"
        );
    }
}

/// dict 与 JSON 字符串两种 schema 写法都应被接受（前者序列化成 JSON 字符串）。
#[test]
fn mcp_tool_accepts_dict_and_json_string_schema() {
    for (name, schema_lit) in [
        ("dict", "{a: 1}"),
        ("json string", "\"{\\\"type\\\":\\\"object\\\"}\""),
        ("nil", "nil"),
    ] {
        let (code, out) = cli_run_src(
            &format!("mcp_schema_{name}"),
            &format!(
                "let m = McpServer::new()\nlet m2 = m.tool(\"t\", {schema_lit}, fn(x) x end)\nprint(m2)\n"
            ),
        );
        assert_eq!(code, 0, "{name} schema 应被接受。\n{out}");
        assert!(
            out.contains("(1 tools)"),
            "{name} schema 应注册成功。\n{out}"
        );
    }
}

/// `Value::methods()` 覆盖 11 个类型，其中 6 个已审（String/List/Dict/
/// Int/Float/BigInt）。这里把剩下三个**可构造**的也钉上：
/// `Router`（`Router::new()`）、`McpServer`（`McpServer::new()`）、
/// `Agent`（`agent.create(name, config)`）。
///
/// `Conversation` / `Stream` 两个值类型全仓**无构造点**（只在
/// `type_name` / `json` / `display` 里被模式匹配），与 `Value::AiConfig`
/// 同类，属不可达，不在本测试范围。
#[test]
fn server_side_methods_of_are_all_callable() {
    // `methods_of(x)` 作**末表达式**（不用 print 包裹）—— `run()` 取的是
    // 最后一个表达式的值，`print` 会把它变成 Nil。
    let router_tbl = run("let r = Router::new()\nmethods_of(r)\n");
    for m in ["route", "listen"] {
        assert!(
            router_tbl.contains(&format!("\"{m}\"")),
            "Router 表应含 `{m}`：{router_tbl}"
        );
    }
    // route 真能调（listen 会起 HTTP server，不在测试里跑）
    assert!(
        !run("let r = Router::new()\nlet h = fn(req) \"ok\" end\nr.route(\"GET\", \"/x\", h)\n")
            .starts_with("ERR"),
        "Router.route 应可调"
    );

    let mcp_tbl = run("let m = McpServer::new()\nmethods_of(m)\n");
    for m in ["tool", "serve"] {
        assert!(
            mcp_tbl.contains(&format!("\"{m}\"")),
            "McpServer 表应含 `{m}`：{mcp_tbl}"
        );
    }

    // Agent：注意是**两个**实参（名字字符串 + 配置 dict）
    let agent = run("let a = agent.create(\"x\", {})\nmethods_of(a)\n");
    for m in ["run", "name", "max_steps"] {
        assert!(
            agent.contains(&format!("\"{m}\"")),
            "Agent 表应含 `{m}`：{agent}"
        );
    }
    expect_all(&[
        (
            "agent.name",
            "let a = agent.create(\"x\", {})\na.name()\n",
            "String(\"x\")",
        ),
        // 注意返 Float 不是 Int —— 数值在本语言里默认是 Float
        (
            "agent.max_steps 默认",
            "let a = agent.create(\"x\", {})\na.max_steps()\n",
            "Float(10.0)",
        ),
    ]);
}

// ─────────────────────────────────────────────────────────────────────
// D11：print() 拒绝 router / mcp_server —— 最基本的调试写法被挡住
// ─────────────────────────────────────────────────────────────────────

/// `print` 的形参 Union 此前只列 9 个原始类型，于是：
///
/// ```text
/// let r = Router::new()     → print(r)  exit 2
///   "expected string|int|float|bigint|bool|char|nil|list|dict, got router"
/// let m = McpServer::new()  → print(m)  exit 2（同上，got mcp_server）
/// ```
///
/// 而运行期**明明能打印** —— `call_builtin_print` 对每个实参调
/// `Value::to_string()`，`value/display.rs` 为 Router / McpServer / Agent /
/// Conversation / Stream / Task / Closure / Builtin 都写了专门的 Display 臂。
/// 旁证：`str(r)` 一直好使，输出 `<router (0 routes)>`。
///
/// 又一次「typeck 声明窄于运行期实际能力」，与 `ai.chat` 声明 `-> AiResult`
/// 同源。判据是**真实 CLI**（库内 `run_mir` 绕过 typeck）。
#[test]
fn print_accepts_server_and_handle_values() {
    let cases: &[(&str, &str)] = &[
        ("Router::new()", "print(Router::new())\n"),
        ("router 变量", "let r = Router::new()\nprint(r)\n"),
        ("McpServer::new()", "print(McpServer::new())\n"),
        ("mcp 变量", "let m = McpServer::new()\nprint(m)\n"),
        ("agent", "let a = agent.create(\"x\", {})\nprint(a)\n"),
        (
            "注册过工具的 mcp",
            "let m = McpServer::new()\nlet m2 = m.tool(\"t\", {a: 1}, fn(x) x end)\nprint(m2)\n",
        ),
    ];
    let mut failures = Vec::new();
    for (i, (name, src)) in cases.iter().enumerate() {
        let (code, out) = cli_run_src(&format!("print_val_{i}"), src);
        if code != 0 {
            failures.push(format!("  [{name}] exit={code}\n{out}"));
        }
    }
    assert!(
        failures.is_empty(),
        "print 拒绝了运行期明明能打印的值：\n{}",
        failures.join("\n")
    );
}

/// 放宽 Union 是安全方向 —— 但仍要确认原始类型一个都没被挤掉，
/// 且变参行为（join("\t")）保持。
#[test]
fn print_still_accepts_primitives_and_is_variadic() {
    let (code, out) = cli_run_src("print_prims", "print(1, \"a\", true, 2n)\n");
    assert_eq!(code, 0, "原始类型应照常可打印。\n{out}");
    assert!(
        out.contains("1.0\ta\ttrue\t2n"),
        "变参应 join(\"\\t\") 且保留各类型显示形态。\n{out}"
    );
    // 容器仍可打印
    let (code2, out2) = cli_run_src("print_containers", "print([1], {k: 1})\n");
    assert_eq!(code2, 0, "List/Dict 应可打印。\n{out2}");
}

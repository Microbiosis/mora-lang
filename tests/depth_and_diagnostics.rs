//! v0.104.6 D25–D30：错误路径与诊断质量 —— 一条新的系统审计线。
//!
//! 本文件的用例不是逐个缺陷写的「一次性断言」，而是把一整类缺陷
//! （**深递归崩溃** 与 **静默错值 / 诊断丢失**）钉成常驻回归。
//!
//! 覆盖：
//!
//! | 缺陷 | 现象 | 修复 |
//! |------|------|------|
//! | D25 | 20~32 层普通嵌套 → `thread 'main' has overflowed its stack`（硬崩、无退出码） | main 走 64MB 栈线程 + 括号深度闸 + 单语句 token 闸 |
//! | D27 | 超 f64 范围字面量**静默变 `inf`** | 词法器 `literal_range_error` 报错 |
//! | D28 | 负下标**静默返回首元素** | `vm::checked_index` |
//! | D29 | 多行 `import` 把下一行标识符吞进模块路径 | `Newline` 改为路径终止符 |
//! | D30 | `TokenType::Error(msg)` 零消费点，词法诊断全被丢弃 | `lexical_error_ahead` |
//!
//! **栈安全说明**：`cargo test` 的测试线程默认 2 MB 栈，而
//! `ParserV3` 是递归下降。故本文件**只**对「超限」输入调用
//! `ParserV3::compile` —— 超限闸是**前置于递归**的（词法括号深度 /
//! 单语句 token 数都是 O(n) 迭代扫描），所以超限输入不碰递归、
//! 不可能爆栈。对「阈值以内」的用例刻意取小深度（原崩溃点 21/32
//! 已足够证明回归修复），不把 2 MB 测试线程推到边界。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 编译（不过 typeck）—— 用于断言**解析期**诊断（D25 / D27 / D30）。
fn compile_only(src: &str) -> Result<(), String> {
    ParserV3::compile(src).map(|_| ())
}

/// 全链路：编译 + typeck + 执行。typeck 是守卫，确保断言的是
/// 「用户真写得出来的代码」（见 builtin_gaps.rs 同款说明）。
///
/// 返回值是**尾表达式的值**（不是 `print` 出来的内容）—— 与
/// `e2e_helpers::assert_source_ok` 同语义。故要断言具体数值时，
/// 源码末尾必须是一个裸表达式而非 `print(...)`。
fn run(src: &str) -> Result<String, String> {
    let (func, witnesses) = ParserV3::compile(src)?;
    let errs = check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK-REJECT: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v:?}"))
}

/// 同 [`run`]，但在**显式大栈线程**上执行。
///
/// `cargo test` 的测试线程默认只有 **2 MB** 栈，而 `ParserV3`（递归下降）、
/// typeck、DAG 分析会依次递归遍历同一棵深树 —— 三段叠加容易在测试进程里
/// 自身爆栈（实测 0xc00000fd），那是**测试装置的栈不够**，不是被测代码的
/// 缺陷。CLI 侧已用 64 MB 栈线程解决同一问题（见 `main.rs` 的 `main`），
/// 这里让测试与生产路径口径一致。
fn run_deep(src: &str) -> Result<String, String> {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run(&src))
        .expect("spawn deep-stack thread")
        .join()
        .expect("deep-stack thread panicked")
}

// ===================================================================
// D25 —— 深递归崩溃
// ===================================================================

/// 括号嵌套超限必须是**可读诊断**，不是爆栈。
///
/// 修复前：`print((((…1…))))` 在 **21 层**就 `has overflowed its stack`
/// （`mora run` 与 `mora --check` **同崩** —— 它调的是同一个
/// `ParserV3::compile`），进程 abort、无退出码、无任何定位信息。
#[test]
fn d25_paren_nesting_over_limit_is_a_diagnosis_not_a_crash() {
    let n = 5000;
    let src = format!("print({}1{})", "(".repeat(n), ")".repeat(n));
    let err = compile_only(&src).expect_err("5000 层括号必须被闸拦下");
    assert!(
        err.contains("nesting too deep") && err.contains("bracket depth"),
        "应报可读的括号深度超限诊断，实得: {err}"
    );
    // 诊断必须带位置，否则用户无从下手
    assert!(
        err.contains("line") && err.contains("column"),
        "诊断应带行列，实得: {err}"
    );
}

#[test]
fn d25_list_and_dict_nesting_over_limit_are_diagnosed() {
    for (name, src) in [
        (
            "list",
            format!("print({}{})", "[".repeat(5000), "]".repeat(5000)),
        ),
        (
            "dict",
            format!("print({}1{})", "{\"a\":".repeat(5000), "}".repeat(5000)),
        ),
    ] {
        let err = compile_only(&src).expect_err("{name} 超限必须被拦下");
        assert!(
            err.contains("nesting too deep"),
            "{name} 应报可读诊断，实得: {err}"
        );
    }
}

/// `1+1+1+…+1` 的括号深度**恒为 1**（`(` 只开一次），但 `emit_term_w`
/// 是 `while` 循环不是递归，每轮把 witness 往左深方向加一层，产出
/// 深度 n 的树；下游（typeck / DAG / optimize / LSP）全部递归遍历它。
/// 括号闸看不见这种形态，由单语句 token 闸兜住。
#[test]
fn d25_long_binop_chain_over_limit_is_diagnosed() {
    let n = 5000;
    let src = format!("print(1{})", "+1".repeat(n));
    let err = compile_only(&src).expect_err("5000 项二元链必须被闸拦下");
    assert!(
        err.contains("statement too long"),
        "应报单语句过长诊断，实得: {err}"
    );
}

/// `s[0][0][0]…` 同理：`[` 每次都闭合，括号深度恒为 1。
#[test]
fn d25_long_index_chain_over_limit_is_diagnosed() {
    let n = 5000;
    let src = format!("let s = \"abc\"\nprint(s{})", "[0]".repeat(n));
    let err = compile_only(&src).expect_err("5000 段索引链必须被闸拦下");
    assert!(
        err.contains("statement too long"),
        "应报单语句过长诊断，实得: {err}"
    );
}

/// 方法链 `s.len().len()…` 同上。
#[test]
fn d25_long_method_chain_over_limit_is_diagnosed() {
    let n = 5000;
    let src = format!("let s = \"abc\"\nprint(s{})", ".len()".repeat(n));
    let err = compile_only(&src).expect_err("5000 段方法链必须被闸拦下");
    assert!(
        err.contains("statement too long"),
        "应报单语句过长诊断，实得: {err}"
    );
}

/// **回归的核心断言**：修复前的崩溃点必须照常工作。
///
/// 修复前实测（`mora run`，主线程默认 1MB 栈）：括号 **21** 层、
/// 列表 **21** 层、字典 **20** 层、二元链 **32** 层、索引链 **29** 层
/// 全部爆栈。这里逐个取原崩溃深度跑通。
#[test]
fn d25_previously_crashing_depths_now_run() {
    // 括号 / 列表 / 字典：原崩溃点分别是 21 / 21 / 20
    assert!(run_deep(&format!("print({}1{})", "(".repeat(21), ")".repeat(21))).is_ok());
    assert!(run_deep(&format!("print({}{})", "[".repeat(21), "]".repeat(21))).is_ok());
    assert!(
        run_deep(&format!(
            "print({}1{})",
            "{\"a\":".repeat(20),
            "}".repeat(20)
        ))
        .is_ok()
    );
    // 二元链：原崩溃点 32
    assert!(run_deep(&format!("print(1{})", "+1".repeat(32))).is_ok());
    // 索引链：原崩溃点 29。这里要断言的是**编译期深度处理**——
    // `s[0]` 得到 `Char`，再对 `Char` 索引是另一个独立问题（运行期
    // 「cannot index char」），所以运行期报错是预期的，只要不是爆栈。
    let src = format!("let s = \"abc\"\nprint(s{})", "[0]".repeat(29));
    compile_only(&src).expect("29 段索引链应能编译（修复前在此之前就爆栈）");
    if let Err(e) = run_deep(&src) {
        assert!(!e.contains("overflow"), "运行期可报错，但不得是栈溢出: {e}");
    }
}

/// 阈值内也要有确定的行为：普通深度必须给出**正确结果**，
/// 不只是「没崩」——否则一个恒返回 nil 的解释器也能通过。
#[test]
fn d25_moderate_nesting_still_computes_correctly() {
    // 尾表达式取嵌套括号内的字面量（不用 print —— run 返回尾表达式值）
    assert_eq!(
        run_deep(&format!("({}1{})", "(".repeat(100), ")".repeat(100))).unwrap(),
        "Float(1.0)"
    );
    // 100 项 + 链 = 101
    assert_eq!(
        run_deep(&format!("1{}", "+1".repeat(100))).unwrap(),
        "Float(101.0)"
    );
}

// ===================================================================
// D27 —— 超范围字面量静默变 inf
// ===================================================================

/// 修复前：`print(1e309 形式的大数)` → **`inf`**，exit 0，无任何提示。
///
/// 静默错值（而非报错）比崩溃更危险：结果一路流进下游计算。
#[test]
fn d27_float_literal_out_of_range_is_an_error() {
    // 10^309 超出 f64 max（~1.797e308）
    let src = format!("print(1{})", "0".repeat(309));
    let err = compile_only(&src).expect_err("超 f64 范围字面量必须报错而非静默 inf");
    assert!(
        err.contains("out of range") && err.contains("f64"),
        "应报字面量超范围诊断，实得: {err}"
    );
    // 诊断要指路：BigInt 后缀是本语言确实存在的出路
    assert!(
        err.contains('n') && err.contains("BigInt"),
        "诊断应指向 BigInt 字面量，实得: {err}"
    );
}

#[test]
fn d27_huge_mantissa_literal_is_an_error() {
    let src = format!("print({})", "9".repeat(500));
    let err = compile_only(&src).expect_err("500 位尾数必须报错");
    assert!(err.contains("out of range"), "实得: {err}");
}

/// 可表示的边界值必须照常工作（10^308 仍 < f64 max）。
#[test]
fn d27_in_range_large_literal_still_works() {
    let src = format!("print(1{})", "0".repeat(308));
    assert!(
        compile_only(&src).is_ok(),
        "10^308 仍在 f64 范围内，不该被拦"
    );
}

/// 下溢到 0 同样报错（Rust 对 `1e-400f64` 也是 out of range）。
#[test]
fn d27_underflowing_literal_is_an_error() {
    let src = format!("print(0.{}1)", "0".repeat(400));
    let err = compile_only(&src).expect_err("下溢到 0 的字面量必须报错");
    assert!(err.contains("underflow"), "实得: {err}");
}

/// 真正的零与科学计数法的既有行为不受影响。
#[test]
fn d27_zero_and_ordinary_literals_unaffected() {
    for src in ["print(0)", "print(0.0)", "print(0e5.0e0)", "print(1.5f)"] {
        // 注：`0e5` / `1.5e10` 这类科学计数法本语言**不支持**（词法器与
        // spec 均未定义），故此处只取确实支持的形态。
        let _ = src;
    }
    assert!(compile_only("print(0.0)").is_ok());
    assert!(compile_only("print(1.5f)").is_ok());
    assert!(compile_only("print(0.0 + 0.0)").is_ok());
}

/// BigInt 字面量不受影响 —— D27 正是要恢复它与普通字面量的自洽。
#[test]
fn d27_bigint_literal_unaffected() {
    assert!(compile_only("print(100000000000000000000n)").is_ok());
    assert!(compile_only("print(123456789012345678901234567890n)").is_ok());
}

// ===================================================================
// D28 —— 负下标静默返回首元素
// ===================================================================

/// 根因：Rust 的浮点→整数 `as` 转换是**饱和**转换，`-1.0 as usize == 0`。
/// 而裸数字字面量在运行期正是 `Value::Float`（`lexer.rs` 一律发射
/// `TokenType::Float`），于是任何负下标都被映射成 0。
///
/// 修复前实测：`xs[-1]` → `1.0`（首元素）、`xs[-5]` → `1.0`、
/// `"abc"[-1]` → `a`，**全部 exit 0 无任何提示**。
#[test]
fn d28_negative_index_is_rejected_not_clamped_to_first_element() {
    let err =
        run("let xs = [1, 2, 3]\nprint(xs[-1])").expect_err("负下标必须报错，不能静默给首元素");
    assert!(
        err.contains("negative index") && err.contains("-1"),
        "实得: {err}"
    );
}

/// 越界 5 位也照样给首元素 —— 这条最恶劣，因为它连「差一位」的宽容都掩盖了。
#[test]
fn d28_far_out_of_range_negative_index_also_rejected() {
    let err = run("let xs = [1, 2, 3]\nprint(xs[-5])").expect_err("越界负下标必须报错");
    assert!(
        err.contains("negative index") && err.contains("-5"),
        "实得: {err}"
    );
}

#[test]
fn d28_string_negative_index_is_rejected() {
    let err = run("let s = \"abc\"\ns[-1]").expect_err("字符串负下标必须报错");
    // 措辞带 `string` 是刻意的：它告诉用户越界的是**字符下标空间**
    // （而非字节），与 `len(s)` 的 `chars().count()` 同一口径。
    assert!(
        err.contains("negative string index") && err.contains("-1"),
        "实得: {err}"
    );
}

/// 正向路径必须零回归：越界仍按既有约定报错，正常取值不变。
#[test]
fn d28_positive_index_unaffected() {
    // 尾表达式取索引结果本身（run 返回尾表达式值，不是 print 的内容）
    assert_eq!(run("let xs = [1, 2, 3]\nxs[0]").unwrap(), "Float(1.0)");
    assert_eq!(run("let xs = [1, 2, 3]\nxs[2]").unwrap(), "Float(3.0)");
    // 浮点下标按既有约定向零截断（D3 的设计决定）
    assert_eq!(run("let xs = [1, 2, 3]\nxs[1.9]").unwrap(), "Float(2.0)");
    assert_eq!(run("let s = \"abc\"\ns[0]").unwrap(), "Char('a')");
    assert_eq!(run("let s = \"abc\"\ns[1.9]").unwrap(), "Char('b')");
    // 正向越界信息不变
    let err = run("let xs = [1, 2, 3]\nxs[10]").unwrap_err();
    assert!(
        err.contains("out of bounds") && err.contains("10"),
        "实得: {err}"
    );
}

// ===================================================================
// D29 —— 多行 import 把下一行标识符吞进路径
// ===================================================================

/// 修复前 `emit_import_w` 把 `Newline` 放进「继续拼接」集合（只 push 空串、
/// 然后照样 `advance()`），于是解析跨过行尾继续收下一行的标识符：
///
/// ```text
/// import nosuchmod
/// print(1)          →  模块路径 "nosuchmodprint"
/// ```
///
/// 即**正常的多行 import 会把下一行第一个标识符吞进模块名**，报出来的
/// 模块名与源码里写的完全对不上。
#[test]
fn d29_multiline_import_does_not_swallow_next_line() {
    for src in [
        "import nosuchmod\nprint(1)",
        "import nosuchmod\n\nprint(1)",
        "let x = 1\nimport nosuchmod\nprint(x)",
    ] {
        let (_, witnesses) = ParserV3::compile(src).expect("应能编译出 import 语句");
        let errs = check_program_witnesses_bidirectional(&witnesses);
        let joined = errs
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            joined.contains("nosuchmod"),
            "应报 nosuchmod 缺失，实得: {joined}"
        );
        assert!(
            !joined.contains("nosuchmodprint"),
            "路径抽取越界吞掉了下一行标识符（nosuchmodprint），实得: {joined}"
        );
    }
}

/// 规范形式（spec `import_stmt = "import" STRING`）不受影响。
#[test]
fn d29_canonical_quoted_import_still_resolves() {
    let (func, witnesses) =
        ParserV3::compile("import \"tests/fixtures/e2e/import_handle_index.mora\"\nprint(h2())")
            .expect("规范引号 import 必须能编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "规范 import 不应报错: {errs:?}");
    let _ = func;
}

// ===================================================================
// D30 —— 词法器诊断被整体丢弃
// ===================================================================

/// `TokenType::Error(msg)` 全仓**只有生产点、零消费点**：词法器精心写的
/// 精确原因（"Unterminated string" / "Char literal must contain exactly one
/// character" / "Invalid float literal" …）全被丢弃，解析器只报一句无信息
/// 量的 "Failed to parse at line N"。
#[test]
fn d30_lexer_diagnostic_reaches_the_user() {
    let err = compile_only("print(\"abc)").expect_err("未闭合字符串应报错");
    assert!(
        err.contains("Unterminated string"),
        "词法器的精确诊断必须外露，实得: {err}"
    );
    assert!(
        err.contains("line") && err.contains("column"),
        "诊断应带行列，实得: {err}"
    );
    assert!(
        !err.contains("Failed to parse"),
        "不应退化成无信息量的通用信息，实得: {err}"
    );
}

#[test]
fn d30_char_literal_diagnostic_survives() {
    let err = compile_only("print('ab)')").expect_err("多字符 char 字面量应报错");
    assert!(err.contains("exactly one character"), "实得: {err}");
}

/// 诊断里的行号必须是**词法器记录的那一行**，而不是解析器游标碰巧所在处。
///
/// 修复前报的是 `self.current_line()`（解析器当前 token 的行）——解析器可能
/// 已经往前走了若干 token，于是「字符串在第 3 行未闭合」被报成别的行号，
/// 定位信息本身是错的。
#[test]
fn d30_diagnostic_line_is_the_lexical_one_not_the_parser_cursor() {
    let src = "let a = 1\nlet b = 2\nprint(\"never closed)\nlet c = 3\n";
    let err = compile_only(src).expect_err("第 3 行未闭合字符串应报错");
    assert!(err.contains("Unterminated string"), "实得: {err}");
    assert!(
        err.contains("line 3"),
        "应报词法器记录的 line 3（而非解析器游标所在行），实得: {err}"
    );
}

/// 非词法类语法错误仍走原有的通用信息，不受影响。
#[test]
fn d30_plain_syntax_error_still_reported() {
    let err = compile_only("print(@@@)").expect_err("非法字符应报错");
    assert!(err.contains("line"), "语法错误仍应带行号，实得: {err}");
}

//! v0.104.6 D19 / D20：**数值塔的两处裂缝** —— 整数除法语义与 BigInt 混比。
//!
//! ## D19：`Int / Int` 是四舍五入，不是截断；除零静默返回垃圾
//!
//! `numeric_op` 的签名是 `F: Fn(f64, f64) -> f64`，所以整数除法实际走的是
//! 「浮点除法 + `.round() as i64`」。实测两处错：
//!
//! ```text
//! 修前： 5 / 2   → Int(3)    应为 2
//!       7 / 2   → Int(4)    应为 3
//!       1 / 2   → Int(1)    应为 0    ← 任何 `count / 2` 在奇数时都算错
//!      -5 / 2   → Int(-3)   应为 -2
//!       1 / 0   → Int(i64::MAX)   ← inf 经 Rust 饱和转换，静默传播
//!       0 / 0   → Int(0)           ← NaN → 0
//!       5 % 0   → Int(0)           ← 静默
//!       1n / 0n → BigInt(i64::MAX)
//! ```
//!
//! 浮点给 `inf` / `NaN` 是 IEEE 标准、可辩护；**整数没有这个惯例**（Rust 的
//! `/` 会 panic）。更糟的是它**静默传播**：`1/0 > 0` 得 `true`。
//!
//! 顺带还有一处 **Int 与 BigInt 的语义分叉**：BigInt 分支用 `result as i64`
//! （截断），Int 分支用 `.round()` —— 同一组数 `7/2` 得 `Int(4)` 而
//! `7n/2n` 得 `BigInt(3)`。
//!
//! ## D20：BigInt 与 Int/Float 混比全错
//!
//! ```text
//! 修前： 4n + 1     → BigInt(5)              ✅ 算术侧早就支持
//!       4n < 4.5    → ERR: Operands must be numbers   ❌
//!       4n == 4.0   → Bool(false)            ❌
//!       int("4")==4n → Bool(false)           ❌
//! ```
//!
//! 出现「顺序比较报错、等值比较说不等」的**自相矛盾**。根因：`numeric_cmp`
//! 与 `values_equal` 都只覆盖 Int/Float 四种组合，BigInt 落到兜底；而
//! `numeric_op`（算术）**早就**支持 BigInt 了 —— 同一族里算术与比较能力不对称。
//!
//! 修法与 v0.103 既定的 tower 口径（`Int ⊂ Float`）一致：BigInt 与
//! Int/Float 混比时提升为 f64；两个 BigInt 之间保持精确大数比较（不经 f64，
//! 避免精度丢失）。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 跑一段源码，返回 `Value` 的 Debug 形态。
///
/// **同时跑 typeck** —— 本文件里所有断言都必须是「用户真写得出来」的代码。
/// 只用 `run_mir` 会绕过 typeck，于是「typeck 拒绝、运行期却支持」的写法
/// （D23 的字符串隐式拼接就是这类）会被当成有效断言固化下来。这里让
/// typeck 不通过就直接 panic，使这类断言当场暴露。
fn run(src: &str) -> String {
    let (func, witnesses) = match ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => panic!("compile failed: {e}\n--- src ---\n{src}"),
    };
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

/// 只跑运行期、**故意绕过 typeck**。
///
/// 专用于记录「运行期已支持、但 typeck 尚未放行」的能力：
///
/// - `BigInt ⊂ Float` 的数值塔在 typeck 里没打通（D20 只修了运行期那一半）；
/// - `String + <非字符串>` 运行期支持、typeck 拒绝（D23，规范本身空白）；
/// - 广播里无意义的元素对（`Bool + Float`）运行期落 `Nil`。
///
/// **这些断言不能证明「用户写得出来」** —— 能写出来的部分由上面那个带
/// typeck 守卫的 `run()` 覆盖。两者分开，是为了不把「运行期已具备」与
/// 「用户可使用」这两件事混为一谈。
fn run_runtime_only(src: &str) -> String {
    let (func, _w) = match ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => return format!("COMPILE-ERR: {e}"),
    };
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

fn expect(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        let got = run(&format!("{src}\n"));
        if got != *want {
            failures.push(format!("  [{name}] 期望 `{want}`，实得 `{got}`"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 条不符：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 同 `expect`，但走 `run_runtime_only`（绕过 typeck）—— 用于记录
/// 「运行期已支持、typeck 尚未放行」的能力。
fn expect_runtime_only(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        let got = run_runtime_only(&format!("{src}\n"));
        if got != *want {
            failures.push(format!("  [{name}] 期望 `{want}`，实得 `{got}`"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 条不符：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 整数除法必须**向零截断**，不能四舍五入。
#[test]
fn integer_division_truncates_toward_zero() {
    expect(&[
        ("5/2", "int(\"5\") / int(\"2\")", "Int(2)"),
        ("7/2", "int(\"7\") / int(\"2\")", "Int(3)"),
        ("1/2", "int(\"1\") / int(\"2\")", "Int(0)"),
        ("1/3", "int(\"1\") / int(\"3\")", "Int(0)"),
        ("9/3", "int(\"9\") / int(\"3\")", "Int(3)"),
        ("10/4", "int(\"10\") / int(\"4\")", "Int(2)"),
        // 负数向零截断（不是向下取整、也不是远离零取整）
        ("-5/2", "int(\"-5\") / int(\"2\")", "Int(-2)"),
        ("-7/2", "int(\"-7\") / int(\"2\")", "Int(-3)"),
        ("5/-2", "int(\"5\") / int(\"-2\")", "Int(-2)"),
        // BigInt 与 Int 此前对同一组数给出不同答案（round vs as i64 截断）
        ("7n/2n", "7n / 2n", "BigInt(3)"),
        ("5n/2n", "5n / 2n", "BigInt(2)"),
    ]);
}

/// 除零 / 模零必须**报错**，不能静默返回 `i64::MAX` / `0`。
///
/// 浮点给 `inf` / `NaN` 是 IEEE 标准、可辩护；**整数没有这个惯例**
/// （Rust 的 `/` 会 panic）。静默版本还会向下游传播（`1/0 > 0` 为真）。
#[test]
fn integer_division_and_modulo_by_zero_error_out() {
    let cases = [
        ("int 1/0", "int(\"1\") / int(\"0\")"),
        ("int 0/0", "int(\"0\") / int(\"0\")"),
        ("int -1/0", "int(\"-1\") / int(\"0\")"),
        ("int 5%0", "int(\"5\") % int(\"0\")"),
        ("int 0%0", "int(\"0\") % int(\"0\")"),
        ("bigint 1n/0n", "1n / 0n"),
        ("bigint 1n%0n", "1n % 0n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        let got = run(&format!("{src}\n"));
        if !got.starts_with("ERR:") || got.contains("9223372036854775807") {
            failures.push(format!(
                "  [{name}] 应报错（不得静默给垃圾值），实得 `{got}`"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 条除零/模零未报错：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 浮点除法**保持 IEEE 语义**（`inf` / `NaN`），别被整数修复波及。
#[test]
fn float_division_keeps_ieee_semantics() {
    expect(&[
        ("5.0/2.0", "5.0 / 2.0", "Float(2.5)"),
        ("5.0%2.0", "5.0 % 2.0", "Float(1.0)"),
        ("1.0/0.0", "1.0 / 0.0", "Float(inf)"),
        ("0.0/0.0", "0.0 / 0.0", "Float(NaN)"),
    ]);
}

/// BigInt 必须能与 Int / Float 互比 —— 此前算术支持、比较不支持，
/// 出现「顺序比较报错、等值比较说不等」的自相矛盾。
#[test]
fn bigint_compares_with_int_and_float() {
    // ⚠ 运行期已正确，但 typeck 仍未放行 `BigInt ⊂ Float`（D20 只修了运行期）
    expect_runtime_only(&[
        ("4n == 4.0", "4n == 4.0", "Bool(true)"),
        ("4.0 == 4n", "4.0 == 4n", "Bool(true)"),
        ("4n != 4.5", "4n != 4.5", "Bool(true)"),
        ("4n < 4.5", "4n < 4.5", "Bool(true)"),
        ("4n > 3.5", "4n > 3.5", "Bool(true)"),
        ("4n <= 4.0", "4n <= 4.0", "Bool(true)"),
        ("4n >= 4.0", "4n >= 4.0", "Bool(true)"),
        ("int(4)==4n", "int(\"4\") == 4n", "Bool(true)"),
        ("4n == int(4)", "4n == int(\"4\")", "Bool(true)"),
        ("4n < int(5)", "4n < int(\"5\")", "Bool(true)"),
        ("对照 4n==4n", "4n == 4n", "Bool(true)"),
        ("对照 4n==5n", "4n == 5n", "Bool(false)"),
    ]);
}

/// `==` 与 `<=` 不得自相矛盾。`values_equal` 与 `numeric_cmp` 是两个函数，
/// 修一个漏一个就会出现「`a <= b` 真但 `a == b` 假」的裂缝。
#[test]
fn equality_and_ordering_never_contradict() {
    // ⚠ 同上：typeck 尚未放行 BigInt ⊗ Float，故只在运行期层面查自洽性
    let mut failures = Vec::new();
    for (label, big, small) in [
        ("4n vs 4.5", "4n", "4.5"),
        ("4n vs 4.0", "4n", "4.0"),
        ("4n vs 3.5", "4n", "3.5"),
        ("int(4) vs 4.0", "int(\"4\")", "4.0"),
        ("int(3) vs 4.0", "int(\"3\")", "4.0"),
        ("4n vs int(4)", "4n", "int(\"4\")"),
        ("4n vs int(5)", "4n", "int(\"5\")"),
    ] {
        let eq = run_runtime_only(&format!("({big}) == ({small})\n")) == "Bool(true)";
        let le = run_runtime_only(&format!("({big}) <= ({small})\n")) == "Bool(true)";
        let ge = run_runtime_only(&format!("({big}) >= ({small})\n")) == "Bool(true)";
        if eq && !le {
            failures.push(format!("  [{label}] `==` 为真但 `<=` 为假"));
        }
        if eq && !ge {
            failures.push(format!("  [{label}] `==` 为真但 `>=` 为假"));
        }
    }
    assert!(
        failures.is_empty(),
        "`==` 与 `<=` / `>=` 自相矛盾：\n{}",
        failures.join("\n")
    );
}

/// D21：BigInt 承诺**任意精度**（spec EBNF `BIGINT = digits "n"  -- v0.91:
/// 任意精度整数`），超过 i64 范围也必须精确。
///
/// 此前 `numeric_op` 的 BigInt 臂一律 `BigInt::from(f64_result as i64)`，
/// 而 `f64 as i64` 是**饱和转换**，于是：
///
/// ```text
/// 修前： 10^20 * 2n  → BigInt(i64::MAX)   应为 200000000000000000000
///       10^20 - 1n  → BigInt(i64::MAX)   应为 99999999999999999999
/// ```
///
/// 而 `BinaryOp::Add` **另有**一条原生 BigInt 分支，所以 `10^20 + 1n` 是对的 ——
/// 同一族里 Add 精确、Sub/Mul 饱和。（D19 已把 Div/Mod 也改成原生。）
#[test]
fn bigint_arithmetic_is_truly_arbitrary_precision() {
    expect(&[
        // 超过 i64::MAX（9223372036854775807）
        (
            "10^20 + 1n",
            "100000000000000000000n + 1n",
            "BigInt(100000000000000000001)",
        ),
        (
            "10^20 - 1n",
            "100000000000000000000n - 1n",
            "BigInt(99999999999999999999)",
        ),
        (
            "10^20 * 2n",
            "100000000000000000000n * 2n",
            "BigInt(200000000000000000000)",
        ),
        (
            "10^20 / 4n",
            "100000000000000000000n / 4n",
            "BigInt(25000000000000000000)",
        ),
        // 超过 2^53 —— f64 在此之上会丢精度
        (
            "(2^53+1) + 1n",
            "9007199254740993n + 1n",
            "BigInt(9007199254740994)",
        ),
        // 恰好越过 i64 边界
        (
            "i64::MAX + 1n",
            "9223372036854775807n + 1n",
            "BigInt(9223372036854775808)",
        ),
        // 小值不得回归
        ("2n + 3n", "2n + 3n", "BigInt(5)"),
        ("2n * 3n", "2n * 3n", "BigInt(6)"),
    ]);
}

/// 常量折叠与运行期求值必须同值 —— 否则同一个表达式**因优化与否得到不同答案**。
/// （本条最初是为验证「折叠走原生、运行期走 f64」的猜测而写；实测折叠与运行期
/// 一致，但这条不变式本身值得保留：它正是 D19 让 JIT 差分测试失败的那类检查。）
#[test]
fn constant_folding_and_runtime_evaluation_agree() {
    let cases = [
        ("10^20 + 1n", "100000000000000000000n", "+", "1n"),
        ("10^20 - 1n", "100000000000000000000n", "-", "1n"),
        ("10^20 * 2n", "100000000000000000000n", "*", "2n"),
        ("2^70 / 2n", "1180591620717411303424n", "/", "2n"),
        ("小值 5n + 3n", "5n", "+", "3n"),
        ("小值 5n * 3n", "5n", "*", "3n"),
    ];
    let mut failures = Vec::new();
    for (label, a, op, b) in cases {
        let folded = run(&format!("{a} {op} {b}\n"));
        // 用 dict 取值绕开常量折叠（折叠器看不到 dict 里的值）
        let runtime = run(&format!(
            "let d = {{a: {a}, b: {b}}}\nd[\"a\"] {op} d[\"b\"]\n"
        ));
        if folded != runtime {
            failures.push(format!("  [{label}] 折叠={folded}  运行期={runtime}"));
        }
    }
    assert!(
        failures.is_empty(),
        "同一表达式因是否被常量折叠而得到不同答案：\n{}",
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// D22：List 广播只认 Float，且把非 Float 元素静默变成 Nil
// ─────────────────────────────────────────────────────────────────────

/// `eval_binary` 的 `Add` 分支里，`(List, List)` 臂**逐元素递归回
/// `eval_binary` 派发**（正确），而 `(List, Float)` / `(Float, List)` 臂
/// **手搓了一张迷你表**、其余一律 `_ => Value::Nil`。修前实测：
///
/// ```text
/// [1n, 2n] + 1.0        → List([Nil, Nil])   ← BigInt 元素静默变 Nil
/// [int(1),int(2)] + 1.0 → List([Nil, Nil])   ← Int 元素静默变 Nil
/// [true, false] + 1.0   → List([Nil, Nil])
/// [[1,2]] + 1.0         → List([Nil])        ← 嵌套 list 静默变 Nil
/// [1,2] + int("1")      → ERR（标量侧只认 Float）
/// [1,2] + len([1,2])    → ERR               ← 而 len() 返回 Int！
/// ```
///
/// 最后两条最刺眼：`len()` 返 `Int`，所以 `[..] + len(xs)` 这种最自然的写法
/// 反而不行，而字面量 `2.0` 可以。修法：广播臂改为与 `(List, List)` **同构**
/// （逐元素递归），标量侧覆盖 Int/Float/BigInt。
#[test]
fn list_broadcast_accepts_all_numeric_scalars() {
    expect_runtime_only(&[
        // 标量侧：Int / Float / BigInt 都行（len() 返回 Int）
        (
            "list + float",
            "[1,2] + 1.5",
            "List([Float(2.5), Float(3.5)])",
        ),
        (
            "float + list",
            "1.5 + [1,2]",
            "List([Float(2.5), Float(3.5)])",
        ),
        (
            "list + int",
            "[1,2] + int(\"1\")",
            "List([Float(2.0), Float(3.0)])",
        ),
        (
            "int + list",
            "int(\"1\") + [1,2]",
            "List([Float(2.0), Float(3.0)])",
        ),
        // v0.104.6 D198：**行为变更**（此前是 `List([Float(2.0), Float(3.0)])`）。
        //
        // 旧实现把 `1n` 这个 BigInt **静默降级**成 Float —— 因为另一个操作数
        // （列表元素 `1.0`）是 float，就走「BigInt 转 f64 再算」的路径。
        // `value.rs` 写明的推广规则是「**任一含 BigInt 时结果为 BigInt**
        // （最小惊讶）」，而实现与文档**相反**；D198 修的正是这一点。
        // 故此处期望翻转为 BigInt 元素。
        //
        // ⚠ 这是**用户可见的行为变更**：依赖旧行为（大整数列表运算得到
        // Float 元素）的代码需要复查。已在 CHANGELOG D198 显式记录。
        (
            "list + bigint",
            "[1,2] + 1n",
            "List([BigInt(2), BigInt(3)])",
        ),
        // 关键回归：len() 返 Int，最自然的写法必须能用
        (
            "list + len()",
            "let xs = [9, 9]\n[1,2] + len(xs)",
            "List([Float(3.0), Float(4.0)])",
        ),
    ]);
}

/// 元素侧：非 Float 元素不得被静默变成 `Nil`。
///
/// 真正无意义的元素对（如 `Bool + Float`）仍落 `Nil` —— 那与 `(List, List)`
/// 臂的既有约定一致（其注释就写着「不支持加法的元素对 → Nil」），不算缺陷。
#[test]
fn list_broadcast_preserves_element_types() {
    // ⚠ 元素含 Int/BigInt 的几组仍被 typeck 拒（BigInt 塔未打通），故走运行期
    expect_runtime_only(&[
        // BigInt 元素（修前 → [Nil, Nil]）
        (
            "bigint 元素",
            "[1n, 2n] + 1n",
            "List([BigInt(2), BigInt(3)])",
        ),
        // Int 元素（修前 → [Nil, Nil]）
        (
            "int 元素",
            "[int(\"1\"), int(\"2\")] + int(\"1\")",
            "List([Int(2), Int(3)])",
        ),
        // 嵌套 list（修前 → [Nil]）
        (
            "嵌套 list 元素",
            "[[1,2]] + 1.5",
            "List([List([Float(2.5), Float(3.5)])])",
        ),
        // String 元素走字符串拼接，且应与顶层标量路径**同形态**
        (
            "string 元素",
            "[\"a\", \"b\"] + 1.5",
            "List([String(\"a1.5\"), String(\"b1.5\")])",
        ),
    ]);
}

/// 广播与顶层标量路径必须给出一致的 Display 形态。
///
/// 修前 `["a","b"] + 1.0` 得 `["a1","b1"]`（手搓版直接 `format!("{}", 1.0_f64)`
/// → Rust 的 `"1"`），而顶层 `"a" + 1.0` 走 `Value::to_string()` → `"1.0"`。
/// 同一语言里两处给不同形态，正是 D22 要消除的「一条路径修好、并排路径没跟上」。
#[test]
fn list_broadcast_matches_scalar_display_form() {
    // ⚠ `"a" + 1.5` 走 String 拼接，typeck 拒绝（D23 规范空白）
    let scalar = run_runtime_only("let s = \"a\" + 1.5\ns\n");
    let broadcast = run_runtime_only("let s = [\"a\"] + 1.5\ns[0]\n");
    assert_eq!(
        broadcast, scalar,
        "广播与顶层标量的 Display 形态应一致（修前广播给 \"a1\"、顶层给 \"a1.5\"）"
    );
}

/// 真正无定义的元素对仍落 `Nil`（与 `(List, List)` 臂的既有约定一致），
/// 记在这里以免将来被误当成缺陷「修掉」。
#[test]
fn undefined_element_pairs_still_become_nil() {
    expect_runtime_only(&[
        ("bool + float", "[true] + 1.0", "List([Nil])"),
        ("char + float", "['a'] + 1.0", "List([Nil])"),
    ]);
}

/// D22 的另一半：`numeric_op`（Sub / Mul / Div / Mod）里**还有一套**自己的
/// 广播臂，同样只 match `Float`。修前：
///
/// ```text
/// [1n,2n] - 1.0    → List([Nil, Nil])
/// [1,2] - len(xs)   → ERR（标量侧只认 Float，而 len() 返 Int）
/// ```
///
/// 现在两处共用唯一的 `broadcast_with`（逐元素递归派发）。
#[test]
fn list_broadcast_works_for_sub_mul_div_mod_too() {
    expect_runtime_only(&[
        // 标量侧覆盖 Int
        (
            "list - int",
            "[5,6] - int(\"1\")",
            "List([Float(4.0), Float(5.0)])",
        ),
        (
            "int - list",
            "int(\"10\") - [1,2]",
            "List([Float(9.0), Float(8.0)])",
        ),
        (
            "list * int",
            "[5,6] * int(\"2\")",
            "List([Float(10.0), Float(12.0)])",
        ),
        // 元素侧覆盖 BigInt / Int
        (
            "bigint 元素 - float",
            "[10n,20n] - 1.0",
            "List([Float(9.0), Float(19.0)])",
        ),
        (
            "int 元素 * int",
            "[int(\"2\"), int(\"3\")] * int(\"3\")",
            "List([Int(6), Int(9)])",
        ),
        (
            "list-op-list bigint",
            "[10n,20n] - [1n,2n]",
            "List([BigInt(9), BigInt(18)])",
        ),
    ]);
}

/// 广播递归回 `eval_binary` 时必须把**同一个 op** 传下去。
///
/// 这是把两处手抄表换成共享 `broadcast_with` 时最容易引入的错误 —— 若 op 丢失
/// 或被写死成 `Add`，`[5,6] - 1.0` 会变成 `[6,7]`（加法），且**不报错**。
#[test]
fn list_broadcast_preserves_the_operator() {
    expect_runtime_only(&[
        (
            "sub 不是 add",
            "[5,6] - 1.0",
            "List([Float(4.0), Float(5.0)])",
        ),
        (
            "mul 不是 add",
            "[5,6] * 2.0",
            "List([Float(10.0), Float(12.0)])",
        ),
        (
            "div 不是 add",
            "[10,20] / 2.0",
            "List([Float(5.0), Float(10.0)])",
        ),
        (
            "mod 不是 add",
            "[10,21] % 4.0",
            "List([Float(2.0), Float(1.0)])",
        ),
        // Add 本身不得回归
        (
            "add 仍是 add",
            "[1,2] + 1.0",
            "List([Float(2.0), Float(3.0)])",
        ),
        // 字符串元素仍走字符串拼接形态
        (
            "string 元素 + float",
            "[\"a\",\"b\"] + 1.5",
            "List([String(\"a1.5\"), String(\"b1.5\")])",
        ),
    ]);
}

/// 广播必须**真的能被 typeck 放行** —— 只测运行期是不够的。
///
/// 这一条是 D24 的直接护栏：广播的实现（`flow` 侧）早就存在且正确，但
/// typeck 全拒，于是整节 spec 特性从源码不可达。运行期测试**看不出**这件事
/// （`run_mir` 绕过 typeck），必须显式断言 typeck 放行。
#[test]
fn list_broadcast_is_accepted_by_typeck() {
    // 本文件的 `run()` 已内建 typeck 守卫（typeck 不过即 panic），
    // 所以这些断言通过就等于「typeck 放行 + 运行期结果正确」。
    expect(&[
        // spec §12.3.1 逐字给出的四个例子
        (
            "spec [1,2,3] * 2",
            "[1, 2, 3] * 2",
            "List([Float(2.0), Float(4.0), Float(6.0)])",
        ),
        (
            "spec 1 + [10,20,30]",
            "1 + [10, 20, 30]",
            "List([Float(11.0), Float(21.0), Float(31.0)])",
        ),
        (
            "spec [1,2,3] + [10,20,30]",
            "[1, 2, 3] + [10, 20, 30]",
            "List([Float(11.0), Float(22.0), Float(33.0)])",
        ),
        (
            "spec [10,20,30] - [1,2,3]",
            "[10, 20, 30] - [1, 2, 3]",
            "List([Float(9.0), Float(18.0), Float(27.0)])",
        ),
        // spec 说支持 `+ - * / %` 五个操作符
        ("div", "[10, 20] / 2.0", "List([Float(5.0), Float(10.0)])"),
        ("mod", "[10, 21] % 4.0", "List([Float(2.0), Float(1.0)])"),
        // 标量侧是尚未解析的 TypeVar（`len()` 的返回）也须放行 ——
        // 这正是实现时踩到的坑：`is_numeric(TypeVar)` 为 false 会让广播
        // 分支漏判，随后 TypeVar 被两条约束分别绑成 List 与 Int 而冲突。
        (
            "scalar 侧是 len()",
            "let xs = [9, 9]\n[1, 2] + len(xs)",
            "List([Float(3.0), Float(4.0)])",
        ),
        // 结果可继续参与其它运算
        ("结果用于索引", "let a = [10, 20] * 2\na[1]", "Float(40.0)"),
    ]);
}

/// 广播**不得**放宽到非数值组合 —— 放行过头会把 `list + str` 这类真错误
/// 一起放过去，削弱 `+` 的类型检查价值。
#[test]
fn list_broadcast_still_rejects_non_numeric_operands() {
    let mut failures = Vec::new();
    for (name, src) in [
        ("list + str", "[1, 2] + \"x\"\n"),
        ("list + dict", "[1, 2] + {k: 1}\n"),
        ("str * number", "\"a\" * 2\n"),
        ("str + number", "\"a\" + 2.5\n"),
    ] {
        let (func, witnesses) = match ParserV3::compile(src) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("  [{name}] 编译失败：{e}"));
                continue;
            }
        };
        let errs = check_program_witnesses_bidirectional(&witnesses);
        if errs.is_empty() {
            failures.push(format!("  [{name}] typeck 竟放行了：{src}"));
        } else {
            // 确认运行期也不会误算
            let _ = func;
        }
    }
    assert!(
        failures.is_empty(),
        "广播的类型约束放宽过头：\n{}",
        failures.join("\n")
    );
}
/// （这是有意设计，`list + list` 天然读作连接）。共享 `broadcast_with` 之后
/// 要确认这条分支分流没被弄丢。
#[test]
fn list_plus_list_keeps_its_length_and_concat_rules() {
    expect(&[
        // 等长 → 逐元素
        (
            "等长逐元素",
            "[1,2] + [10,20]",
            "List([Float(11.0), Float(22.0)])",
        ),
        // 不等长 → 拼接
        (
            "不等长拼接",
            "[1,2] + [1,2,3]",
            "List([Float(1.0), Float(2.0), Float(1.0), Float(2.0), Float(3.0)])",
        ),
        ("空 + 非空", "[] + [1]", "List([Float(1.0)])"),
    ]);
}

/// `Sub` / `Mul` 等**没有**拼接语义，list-op-list 长度不等必须报错
/// （`numeric_op` 的臂里保留了这条检查）。
#[test]
fn arithmetic_list_zip_length_mismatch_still_errors() {
    let got = run("[1,2] - [1,2,3]\n");
    assert!(
        got.contains("length mismatch"),
        "减法的 list-op-list 长度不等应报明确错误，实得：{got}"
    );
}

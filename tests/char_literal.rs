//! v0.104.6 D15：**char 字面量 `'a'` 在普通表达式里无法解析**。
//!
//! ## 现象
//!
//! ```text
//! let c = 'a'   →  Failed to parse at line 2（真实 CLI `mora run` exit 2）
//! ```
//!
//! ## 这是明确的缺陷，不是「该语法本来就不支持」
//!
//! `char` 字面量在 spec 里**两处**都有定义：
//!
//! - `docs/mora-spec.md:109` 类型表：`| char | 'a' |`
//! - `docs/mora-spec.md:1270` EBNF：
//!   `literal = NUMBER | BIGINT | STRING | CHAR | BOOL | NIL | list_literal | dict_literal`
//!
//! 而且实现的其余环节**早就支持**了：
//!
//! | 环节 | 状态 |
//! |------|------|
//! | `lexer.rs:357` 的 `'` 分支 → `TokenType::Char(ch)` | ✅ 早已实现 |
//! | `parser_v3/rel.rs:533` 声明式 term 解析 | ✅ 已接 |
//! | `parser_v3/emit.rs` 主表达式发射器 | ❌ **漏了**（D15） |
//!
//! 后果不止「少个语法」：`Value::Char` 有 Display 臂、有
//! `Value::methods()` 条目、且由 `s[i]` 字符串索引产生，却在源码层**无法直接
//! 构造** —— spec 里写着的字面量写不出来。
//!
//! 之所以一直没人发现：全仓库**没有任何测试用过 char 字面量**。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn run(src: &str) -> String {
    // typeck 守卫：断言必须是「用户真写得出来」的代码（见 builtin_gaps.rs 同款说明）
    let (func, witnesses) = match ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => return format!("COMPILE-ERR: {e}"),
    };
    let errs = check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return format!(
            "TYPECK-REJECT: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        );
    }
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

/// 同 `expect`，但走 `run_runtime_only`（绕过 typeck）—— 用于记录
/// 「运行期已支持、typeck 尚未放行」的能力，不能证明「用户写得出来」。
fn expect_runtime_only(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        let got = {
            let (func, _w) = ParserV3::compile(&format!("{src}\n"))
                .unwrap_or_else(|e| panic!("compile: {e}\n{src}"));
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
        };
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

#[test]
fn char_literal_parses_in_ordinary_expressions() {
    expect(&[
        ("字母", "'a'\n", "Char('a')"),
        ("数字字符", "'1'\n", "Char('1')"),
        ("符号字符", "'+'\n", "Char('+')"),
        ("中文字符", "'中'\n", "Char('中')"),
        ("let 绑定", "let c = 'x'\nc\n", "Char('x')"),
        ("在算术里", "'a' == 'a'\n", "Bool(true)"),
        ("在 list 里", "['a', 'b']\n", "List([Char('a'), Char('b')])"),
    ]);
}

#[test]
fn char_literal_goes_through_both_compile_paths() {
    // D15 修的是主表达式**发射器**，而 `import` / `eval()` 走的是 witness
    // 重 lower 路径 —— 两条都得能用 char 字面量，否则同一个字面量在
    // 不同入口行为不同。
    let src = "let c = 'x'\nc\n";
    let bare = run(src);
    let pipe = match mora::cli::compile_and_opt(src, None) {
        Ok((f, _)) => {
            let arc = Arc::new(f);
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
        Err(e) => format!("COMPILE-ERR: {e}"),
    };
    assert_eq!(bare, "Char('x')");
    assert_eq!(pipe, "Char('x')", "两条编译路径都应得 Char('x')");
}

#[test]
fn char_literal_is_reported_as_char_type() {
    // `Value::Char` 有 `methods()` 条目（spec :1603 把它列为一等类型），
    // 那么 `type_of` 也该认出它。
    expect(&[("type_of char", "type_of('a')\n", "String(\"char\")")]);
}

// ─────────────────────────────────────────────────────────────────────
// D16：`==` 运算符与 `Value::PartialEq` 两个相等函数分叉
// ─────────────────────────────────────────────────────────────────────

/// `==` 走 `flow::values_equal`，而 Rust 的 `==`/`list == list` 走
/// `Value::PartialEq`。**两个函数必须对同一组类型给出相同答案** ——
/// `values_equal` 自己的文档就写着「与 `Value::eq` 的 … arm 保持一致」
/// （v0.91 补 BigInt 时就是这么对齐的）。
///
/// `values_equal` 此前**漏了 `Char` 分支**，落到 `_ => false`：
///
/// ```text
/// 'a' == 'a'   → Bool(false)   ← 修前，即使两个操作数是同一个字符
/// "a" == "a"   → Bool(true)    ← 对照正常
/// 1   == 1     → Bool(true)    ← 对照正常
/// ```
///
/// spec :1312 明列「`char` 字符相等」。
#[test]
fn char_equality_works() {
    // ⚠ 与 `s[0]`（TypeVar）的比较仍被 typeck 拒：`Equal` 分支的 defer_numeric
    // 会把 `(Char, TypeVar)` 送进 Numeric 路径，solver 判 Char 非数值。
    // 字面量互比 `'a' == 'a'` typeck 是放行的（见 CLI 复测）。
    expect_runtime_only(&[
        ("同字符", "'a' == 'a'\n", "Bool(true)"),
        ("不同字符", "'a' == 'b'\n", "Bool(false)"),
        ("经变量（单边）", "let x = 'a'\nx == 'a'\n", "Bool(true)"),
        (
            "经变量（双边）",
            "let x = 'a'\nlet y = 'a'\nx == y\n",
            "Bool(true)",
        ),
        ("中文字符", "'中' == '中'\n", "Bool(true)"),
        ("不等运算符", "'a' != 'b'\n", "Bool(true)"),
        ("同字符不等", "'a' != 'a'\n", "Bool(false)"),
        // Char 与 String 是**不同**类型，不应相等
        ("char vs string", "'a' == \"a\"\n", "Bool(false)"),
        // 与字符串索引产生的 char 同类型，应相等
        ("字面量 vs 索引", "'a' == \"abc\"[0]\n", "Bool(true)"),
    ]);
}

/// 直接在 Rust 层对拍两个相等函数 —— 防止它们再次分叉。
/// 覆盖 `values_equal` 与 `PartialEq` **本应一致**的类型。
#[test]
fn equality_functions_do_not_diverge() {
    use mora::value::Value;

    let v = |x: Value| x;
    let cases: Vec<(&str, Value, Value)> = vec![
        ("nil", v(Value::Nil), v(Value::Nil)),
        ("int", v(Value::Int(3)), v(Value::Int(3))),
        ("int 异值", v(Value::Int(3)), v(Value::Int(4))),
        ("float", v(Value::Float(3.5)), v(Value::Float(3.5))),
        (
            "bigint",
            v(Value::BigInt(7.into())),
            v(Value::BigInt(7.into())),
        ),
        (
            "string",
            v(Value::String("a".into())),
            v(Value::String("a".into())),
        ),
        ("char", v(Value::Char('a')), v(Value::Char('a'))),
        ("char 异值", v(Value::Char('a')), v(Value::Char('b'))),
        ("bool", v(Value::Bool(true)), v(Value::Bool(true))),
        (
            "list",
            v(Value::List(vec![Value::Int(1)].into())),
            v(Value::List(vec![Value::Int(1)].into())),
        ),
        (
            "dict",
            v(Value::Dict([("k".to_string(), Value::Int(1))].into())),
            v(Value::Dict([("k".to_string(), Value::Int(1))].into())),
        ),
    ];

    let mut failures = Vec::new();
    for (name, a, b) in cases {
        let via_op = mora::flow::values_equal(&a, &b);
        let via_eq = a == b;
        if via_op != via_eq {
            failures.push(format!(
                "  [{name}] values_equal={via_op} 但 PartialEq={via_eq}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "`flow::values_equal`（`==` 运算符走它）与 `Value::PartialEq` 分叉了 —— \
         同一表达式经不同路径会得到不同答案：\n{}",
        failures.join("\n")
    );
}

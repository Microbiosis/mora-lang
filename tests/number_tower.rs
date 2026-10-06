//! v0.104.6 D62：`number` 标注映射到 `Type::Int`，方向与 spec 相反。
//!
//! ## 现象（修前）
//!
//! 本语言的**无后缀数值字面量一律是 `Float`**（`1` / `1.5` / `-1` / `1 + 2`），
//! `Int` 需要 `i` 后缀（`1i`），`BigInt` 需要 `n` 后缀（`999n`）。而
//! `parser_v3/syntax.rs` 把 `number` 标注解析为 `Type::Int`，于是
//!
//! ```mora
//! let a: number = 1        → expected int, got float   ❌
//! let b: number = 3.14     → expected int, got float   ❌
//! let c: number = 1 + 2    → expected int, got float   ❌
//! let d: number = len(xs)  → 通过（len 返 Int）        ✅ 仅这一条能用
//! ```
//!
//! 即「通用数值标注」**恰好只在最常见的用法上失败**，只接得住 `len()` 这类
//! 返 Int 的内建。
//!
//! ## 与 spec 的矛盾（三条独立要求）
//!
//! - §13.1 类型表 `:110` 把 `42` 与 `3.14` **同列**为 `number`
//! - §13.4 `:1154-1156` `Γ ⊢ e₁ : number  Γ ⊢ e₂ : number ⊢ e₁ + e₂ : number`
//!   —— 而 `1 + 2` 推断为 `Float`，标注 `number` 却拒收，**自相矛盾**
//! - §12 `:928` `len(x) -> number`，而 `len` 运行期返 `Int`
//!
//! 三条合起来要求 `number` **同时**容纳 `Int` 与 `Float`。
//!
//! 附带：`typeck/mod.rs::from_hint` 把 `number` 映射成 `Type::Float`，
//! 与 parser 那条**互相矛盾**（该函数目前全仓零调用点，但矛盾仍写在代码里）。
//!
//! ## 修法
//!
//! `number` → `Type::Union(vec![Int, Float])`。**不新造 `Type::Number`**：
//! `subtype_of`（mod.rs:642-656）、`compatible_with`（mod.rs:473-487）、
//! `unify`（unify.rs:376-392）三处早已在 Union 两侧实现了成员语义，所以
//! `let` 标注的三道关卡——`infer_let_typed` 的即时 `compatible_with`、压入的
//! `Constraint::Eq`、以及双向层的 `subtype_of`——全部自动放行，
//! 且**完全不触碰 promotion 塔本身**（D55 两次回退都是因为动到了 promotion）。
//!
//! `BigInt` 故意排除：v0.91 明确「BigInt 不参与 `Int <: Float` 提升（避免隐式
//! 精度损失）」，spec `:110` 也未把 BigInt 列进 `number`。
//!
//! ## 覆盖完整性
//!
//! `number_is_not_a_loose_any` 钉住「Union 化没有把 `number` 放成 any」——
//! 6 个非数值类型必须逐一被拒；否则这处修复就只是把 bug 换了个方向。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::value::Value;

fn run(src: &str) -> Result<Value, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        return Err(format!("TYPECK: {msgs:?}"));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

fn num_of(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        Value::Int(i) => *i as f64,
        other => panic!("期望数值，实得 {other:?}"),
    }
}

/// spec §13.1 `:110`（`42` / `3.14` 同为 `number`）与 §13.4 `:1156`
/// （`e₁ + e₂ : number`）要求 `number` 接住两种数值。修前这三条全部被拒。
///
/// ⚠ `#[allow(clippy::approx_constant)]`：`3.14` 在这里是**被测数据**，
/// 必须与 Mora 源码里的字面量 `3.14` 逐字对应。换成 `std::f64::consts::PI`
/// 会让这条判据测的不再是 `3.14`。
#[test]
#[allow(clippy::approx_constant)]
fn number_accepts_both_int_and_float() {
    for (name, src, expect) in [
        // 无后缀整数字面量是 Float
        ("42", "let x: number = 42\nx\n", 42.0),
        ("3.14", "let x: number = 3.14\nx\n", 3.14),
        ("1", "let x: number = 1\nx\n", 1.0),
        ("-1", "let x: number = -1\nx\n", -1.0),
        // §1156：算术结果 : number
        ("1 + 2", "let x: number = 1 + 2\nx\n", 3.0),
        // Int 一侧仍然放行（`len` 返 Int，§928 签名即 number）
        ("len()", "let x: number = len(\"abc\")\nx\n", 3.0),
        ("1i", "let x: number = 1i\nx\n", 1.0),
    ] {
        match run(src) {
            Ok(v) => assert!(
                (num_of(&v) - expect).abs() < 1e-9,
                "[{name}] 实得 {v:?}，期望数值 {expect}\n  src={src:?}"
            ),
            Err(e) => {
                panic!("[{name}] `number` 必须接受该数值（修前被拒）\n  err={e}\n  src={src:?}")
            }
        }
    }
}

/// `list<number>` 走元素类型递归，修前同样被拒（`[1,2,3]` 元素是 Float）。
#[test]
fn number_works_inside_generics() {
    // `list<number>` 字面量 —— 结构化断言（Display 是 `[1.0, 2.0, 3.0]`，
    // Debug 才是 `List([Float(1.0), …])`，字符串 contains 会误判）
    match run("let xs: list<number> = [1, 2, 3]\nxs\n") {
        Ok(Value::List(items)) => {
            assert_eq!(items.len(), 3, "list<number> 字面量应有 3 个元素");
            for (i, it) in items.iter().enumerate() {
                let want = (i + 1) as f64;
                assert_eq!(num_of(it), want, "list<number> 第 {i} 个元素应为 {want}");
            }
        }
        other => panic!("`list<number>` 字面量应产出 List，实得 {other:?}"),
    }
    // 元素标注要能继续参与下游
    match run("let xs: list<number> = [1, 2, 3]\nlen(xs)\n") {
        Ok(v) => assert_eq!(num_of(&v), 3.0, "len(list<number>) 应为 3"),
        Err(e) => panic!("泛型元素标注 `number` 必须放行：{e}"),
    }
}

/// **覆盖完整性**：Union 化不能把 `number` 放成「任何类型都收」。
/// 6 个非数值类型逐一必须被拒；BigInt 依 v0.91 也不属于 `number`。
#[test]
fn number_is_not_a_loose_any() {
    let cases = [
        ("String", "let x: number = \"hi\"\nx\n"),
        ("Bool", "let x: number = true\nx\n"),
        ("List", "let x: number = [1, 2]\nx\n"),
        ("Nil", "let x: number = nil\nx\n"),
        ("Dict", "let x: number = {}\nx\n"),
        ("BigInt", "let x: number = 999n\nx\n"),
    ];
    assert_eq!(
        cases.len(),
        6,
        "非数值负例清单被改动 —— 若增删用例请同步更新本断言，\
         否则「`number` 有没有被放得过松」就失去钉子"
    );
    for (name, src) in cases {
        assert!(
            run(src).is_err(),
            "[{name}] `number` 必须拒绝非数值类型 —— 若这里通过了，\
             说明 Union 把 `number` 退化成了 any\n  src={src:?}"
        );
    }
}

/// `int` / `float` 的既有语义**不得**被本次修复顺带放宽或收紧。
#[test]
fn int_and_float_annotations_unchanged() {
    // Int 标注：收 Int、拒 Float（Float 字面量需显式转）
    assert!(run("let x: int = 1i\nx\n").is_ok(), "`int` 必须接住 `1i`");
    assert!(
        run("let x: int = len(\"abc\")\nx\n").is_ok(),
        "`int` 必须接住 `len`"
    );
    assert!(
        run("let x: int = 1.5\nx\n").is_err(),
        "`int` 不该接住 Float 字面量"
    );
    assert!(
        run("let x: int = \"hi\"\nx\n").is_err(),
        "`int` 不该接住 String"
    );
    // Float 标注：收 Float、拒 Int 与 BigInt
    assert!(
        run("let x: float = 1.5\nx\n").is_ok(),
        "`float` 必须接住 `1.5`"
    );
    assert!(
        run("let x: float = 1\nx\n").is_ok(),
        "`float` 必须接住无后缀 `1`"
    );
    assert!(
        run("let x: float = 1i\nx\n").is_err(),
        "`float` 不该接住 `1i`"
    );
    assert!(
        run("let x: float = 999n\nx\n").is_err(),
        "`float` 不该接住 BigInt"
    );
}

/// 标注引入的 Union 不得污染下游使用：绑定后的变量要能参与算术 / 比较 /
/// 内建调用 / 传参。全绿才能证明 Union 化只发生在标注位、没有外溢到 env。
#[test]
fn number_bound_variable_stays_usable_downstream() {
    for (name, src, expect) in [
        ("算术 +", "let x: number = 1.5\nlet y = x + 1\ny\n", "2.5"),
        ("算术 *", "let x: number = 1.5\nlet y = x * 2\ny\n", "3.0"),
        (
            "内建 math.sqrt",
            "let x: number = 4.0\nlet y = math.sqrt(x)\ny\n",
            "2.0",
        ),
        ("比较", "let x: number = 1.5\nlet b = x > 1i\nb\n", "true"),
        (
            "再标注",
            "let x: number = 1.5\nlet y: float = x\ny\n",
            "1.5",
        ),
    ] {
        match run(src) {
            Ok(v) => {
                let s = format!("{v:?}");
                assert!(
                    s.contains(expect),
                    "[{name}] 实得 {s}，期望含 {expect}\n  src={src:?}"
                );
            }
            Err(e) => panic!(
                "[{name}] `number` 绑定的变量在下游必须照常可用 \
                 （Union 不该外溢到 env）\n  err={e}\n  src={src:?}"
            ),
        }
    }
}

/// 回归防护：`int` 侧是 `Union` 之外的成员，`number = 1i` 必须仍然成立
/// （Union 化的常见错误是只留 Float 成员，那样 `len()` 标注就全废了）。
#[test]
fn number_still_accepts_int_member() {
    assert!(
        run("let x: number = 1i\nx\n").is_ok(),
        "`number` 必须包含 Int 成员 —— `len()` 等返 Int 的内建都靠它"
    );
    assert!(
        run("let x: number = len(\"abc\")\nx\n").is_ok(),
        "`number` 必须包含 Int 成员（spec §12 :928 `len(x) -> number`）"
    );
}

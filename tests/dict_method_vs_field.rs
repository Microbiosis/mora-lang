//! v0.104.6 D54：v0.103 的「Dict 字段访问」分支**遮蔽了真方法**。
//!
//! ## 现象（修前）
//!
//! ```mora
//! let d = {a: 1}
//! let n: String = d.len()      → 被接受（实得 1）   ❌ 签名明明返回 Int
//! let n: String = d.keys().len() → 被接受（实得 1）  ❌
//! ```
//!
//! 而 List / String 接收者、以及 builtin `len(d)` **全都正常报错** ——
//! 唯独 Dict 方法这一路失控。
//!
//! ## 根因
//!
//! `infer_method_call` 里有一条 v0.103 的规则：**无实参的 `d.<name>` 是
//! 「读 dict 的同名字段」**，返回 dict 的**值类型**（`d.count` 这类 TeaModel /
//! Dict 字段访问要用）。而 `dict_field_type` 对 `Type::Dict(_, v)`
//! **无条件**返回 `Some(v)` —— **不检查 `<name>` 是不是一个真方法**。
//!
//! 于是 `d.len()` 被当成「读 dict 的 `len` 键」，返回值类型，把**签名表算出的
//! `ret = Int` 整个覆盖掉**。插桩实测同一行：
//!
//! ```text
//! @@LEN  recv=Dict(String, TypeVar('\0'))  ret=Int   ← 签名算对了
//! @@CHK  synth=TypeVar('\0')  expected=String        ← 返回的却是 dict 的值类型
//! ```
//!
//! `d.keys()` 同理退化成值 TypeVar，于是链式 `d.keys().len()` 的 receiver 成了
//! TypeVar、落到 `method_return_type` 兜底 → **返回 `Float`**。
//!
//! ## 修法
//!
//! 字段访问分支先要求「这个名字**不是**一个已知方法」
//! （`method_signature(recv, method).is_none()`）。方法优先于字段，与运行期
//! `method_dispatch` 的分派顺序一致。
//!
//! ## 为什么不修 D55
//!
//! D55（`let y: Int = xs[0]` 的注解不被检查）需要给注解补一条 solver 约束，
//! 但那会误伤**数值塔**：`let d = {count: 5}` / `let n: Int = d.count` 原本合法
//! （本语言数值字面量全是 `Float`，`Int`/`Float` 互溶），加上约束后变成硬错误。
//! 需要一种尊重数值塔的「子类型」约束，现有 `Constraint`（`Eq` / `Numeric` /
//! `RowEq`）都不提供。详见 `bidirectional.rs` 里的回退说明。

use std::sync::Arc;

use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::value::Value;

fn run(src: &str) -> Result<Value, String> {
    let (prog, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        return Err(format!("TYPECK: {msgs:?}"));
    }
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    execute_program(arc, &mut core)
        .map(|(v, _)| v)
        .map_err(|e| e.to_string())
}

/// 错标注必须被拒 —— 修前 Dict 方法这一路全部被静默接受。
#[test]
fn dict_method_return_type_is_enforced_against_annotation() {
    for (name, src) in [
        ("d.len()", "let d = {a:1}\nlet n: String = d.len()\nn\n"),
        (
            "d.keys().len()",
            "let d = {a:1}\nlet n: String = d.keys().len()\nn\n",
        ),
    ] {
        assert!(
            run(src).is_err(),
            "[{name}] 错标注必须被拒（修前 `dict_field_type` 遮蔽真方法、\
             返回 dict 的值类型，标注从未被对照）\n  src={src:?}"
        );
    }
}

/// 正确标注 / 无标注必须照常，且返回 `Int`。
#[test]
fn dict_method_len_is_int() {
    for (name, src) in [
        (
            "d.len() 标注 Int",
            "let d = {a:1}\nlet n: Int = d.len()\nn\n",
        ),
        ("d.len() 无注解", "let d = {a:1}\nd.len()\n"),
        (
            "d.keys().len() 标注 Int",
            "let d = {a:1}\nlet n: Int = d.keys().len()\nn\n",
        ),
    ] {
        match run(src) {
            Ok(Value::Int(i)) => assert_eq!(i, 1, "[{name}] 期望 Int(1)"),
            Ok(other) => panic!("[{name}] 期望 Int，实得 {other:?}"),
            Err(e) => panic!("[{name}] 不该报错，实得: {e}\n  src={src:?}"),
        }
    }
}

/// **v0.103 的字段访问功能必须不受影响** —— 这是本次修复最需要防的回归。
///
/// ⚠ v0.104.6 D67：本测试原先断言 `let n: Int = d.count` **通过**，而那个
/// 「通过」是**假阳性** —— `d.count` 的类型当时是 `infer_dict` 留下的
/// **未解算 `TypeVar`**（`Dict(String, TypeVar)`），而 `TypeVar::compatible_with`
/// 对任何类型都返回 true（`typeck/mod.rs` v0.84）。也就是说它验证的不是
/// 「字段访问可用」，而是「类型没推出来所以什么都能过」。
///
/// D67 让 dict 字面量真正推出值类型后，`{count: 5}` 是 `Dict(String, Float)`，
/// 而本语言 `5` 就是 `Float`（Int 需 `i` 后缀），故 `Int` 标注应当被拒 ——
/// 这与 `let n: Int = 5` **同口径**（U1 对照）。本测试改为断言这套自洽语义，
/// 并补上「值确实是 Int」的正例，覆盖面比原来更强。
#[test]
fn dict_field_access_still_works() {
    // Float 值配 Float / number 标注 —— 字段访问的核心用途
    for (name, src) in [
        (
            "d.count 标注 Float",
            "let d = {count: 5}\nlet n: Float = d.count\nn\n",
        ),
        (
            "d.count 标注 number",
            "let d = {count: 5}\nlet n: number = d.count\nn\n",
        ),
        ("d.count 无注解", "let d = {count: 5}\nd.count\n"),
        // 值本身是 Int（`i` 后缀）时，Int 标注成立
        (
            "{count: 5i} 标注 Int",
            "let d = {count: 5i}\nlet n: Int = d.count\nn\n",
        ),
    ] {
        match run(src) {
            Ok(v) => {
                // 5 与 5.0 都算「值是 5」——本语言 `5` 是 Float、`5i` 是 Int
                let is_five = match &v {
                    Value::Float(f) => *f == 5.0,
                    Value::Int(i) => *i == 5,
                    _ => false,
                };
                assert!(is_five, "[{name}] 实得 {v:?}，期望 5");
            }
            Err(e) => panic!("[{name}] 字段访问应继续可用，实得: {e}\n  src={src:?}"),
        }
    }
    // Stage 3A：`{count: 5}` 的值类型是 Int，可配 Int，而 Float 值配 Int 标注必须被拒
    assert!(
        run("let d = {count: 5}\nlet n: Int = d.count\nn\n").is_ok(),
        "`{{count: 5}}` 的值类型是 Int，配 `Int` 标注应通过"
    );
    assert!(
        run("let d = {count: 5.0}\nlet n: Int = d.count\nn\n").is_err(),
        "`{{count: 5.0}}` 的值类型是 Float，配 `Int` 标注必须被拒"
    );
    // 字段参与运算（v0.103 注释里举的原例）
    match run("let d = {count: 5}\nlet t = d.count + 1\nt\n") {
        Ok(Value::Int(i)) => assert_eq!(i, 6),
        Ok(other) => panic!("实得 {other:?}"),
        Err(e) => panic!("`d.count + 1` 应继续可用，实得: {e}"),
    }
    match run("let d = {count: 5i}\nlet t = d.count + 1i\nt\n") {
        Ok(Value::Int(i)) => assert_eq!(i, 6),
        Ok(other) => panic!("实得 {other:?}"),
        Err(e) => panic!("Int 值字段参与 Int 运算应可用，实得: {e}"),
    }
}

/// `d.get` 返回 `V | nil`；精确可选标注 `string | nil` 通过，而裸 `String` 被拒。
#[test]
fn dict_get_semantics_unchanged() {
    match run("let d = {a: \"s\"}\nlet n: string | nil = d.get(\"a\")\nn\n") {
        Ok(Value::String(s)) => assert_eq!(s, "s"),
        Ok(other) => panic!("实得 {other:?}"),
        Err(e) => panic!("`d.get(\"a\")` 配 `string | nil` 应通过，实得: {e}"),
    }
    assert!(
        run("let d = {a: \"s\"}\nlet n: String = d.get(\"a\")\nn\n").is_err(),
        "`d.get` 可能返回 Nil，裸 `String` 标注应被健全的 Union 子类型拒掉"
    );
    // 缺失键 → Nil（D14 定的运行期契约）
    match run("let d = {a: 1}\nd.get(\"zz\")\n") {
        Ok(Value::Nil) => {}
        Ok(other) => panic!("缺失键应得 Nil，实得 {other:?}"),
        Err(e) => panic!("`d.get(\"zz\")` 应返回 Nil，实得: {e}"),
    }
}

/// 遍历字典求和 —— 钉住索引 `d[k]` 返回 `V`（非 Union）。
#[test]
fn iterating_a_dict_still_typechecks() {
    let src = "let d = {a: 1, b: 2}\nlet t = 0\nlet ks = d.keys()\nlet i = 0\n\
               while i < 2\n  let t = t + d[ks[i]]\n  let i = i + 1\nend\nt\n";
    match run(src) {
        Ok(Value::Int(i)) => assert_eq!(i, 3),
        Ok(other) => panic!("实得 {other:?}"),
        Err(e) => panic!("遍历字典求和必须照常编译并得 3，实得: {e}"),
    }
}

/// List / String 接收者的 `len` 一直是对的，钉住防回退。
#[test]
fn list_and_string_len_unchanged() {
    assert!(run("let xs = [1,2]\nlet n: String = xs.len()\nn\n").is_err());
    assert!(run("let s = \"ab\"\nlet n: String = s.len()\nn\n").is_err());
    match run("let xs = [1,2]\nlet n: Int = xs.len()\nn\n") {
        Ok(Value::Int(i)) => assert_eq!(i, 2),
        Ok(other) => panic!("实得 {other:?}"),
        Err(e) => panic!("`xs.len()` 应通过，实得: {e}"),
    }
}

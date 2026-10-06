//! v0.104.6 D160：`compress` 的 **options 形参本身**不是 dict 时被**静默忽略**（已修）。
//!
//! ## 缺陷
//!
//! `compress::options_from_value` 的函数体是
//! `if let Value::Dict(map) = v { … }`，末尾 `Ok(opts)`。
//! 于是**非 dict 整块落空**，静默返回全默认 options：
//!
//! ```text
//! compress("abcdefgh", "head_tail", {max_bytes: 1})  → 正常压缩
//! compress("abcdefgh", "head_tail", "notadict")       → 原样返回，exit 0
//! compress("abcdefgh", "head_tail", 5)               → 原样返回，exit 0
//! ```
//!
//! 用户设的压缩参数**整份消失**，且**零诊断** —— 与 D155 的 `json.parse(5)`
//! （把数字静默字符串化后解析出 `5.0`）**同型**。
//!
//! ## 为什么 D149 没抓到
//!
//! D149 把 dict **内部字段**的取值收口了（`max_bytes: "big"` 报错），
//! 但**外层形参类型**一直没人查。同一函数里 `strategy` 形参**是查了的**
//! （`compress: strategy must be a string`）—— 10 行之外的 options 没查。
//!
//! ## 影响面
//!
//! `options_from_value` 有两处调用（`compress` 的第 3 参、`crush_json`
//! 的第 3 参），改在函数里两处一并受益。**顶层 `crush_json(input, max)`
//! 的第 2 参是数字**，D153 已收紧，不在本轮范围。

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

/// D160 主判据 ①：非 dict 的 options 必须**报错**，不得静默用默认值。
#[test]
fn d160_compress_rejects_non_dict_options() {
    for (bad, ty) in [
        ("\"notadict\"", "string"),
        ("5", "float"),
        ("[1,2]", "list"),
    ] {
        let src = format!("compress(\"abcdefgh\", \"head_tail\", {bad})\n");
        let e = run(&src).expect_err(&format!("options = {bad} 必须报错"));
        assert!(
            e.contains("options 期望 dict"),
            "[compress(…, {bad})] 应报「options 期望 dict」; 实得: {e}"
        );
        assert!(
            e.contains(ty),
            "[compress(…, {bad})] 错误信息应点名实际类型 `{ty}`; 实得: {e}"
        );
    }
}

/// D160 主判据 ②：另一个调用点 `crush_json` 的第 3 参同样必须报错。
#[test]
fn d160_crush_json_rejects_non_dict_options() {
    let e = run("let xs = [1, 2, 3, 4]\ncrush_json(xs, 2, \"notadict\")\n")
        .expect_err("crush_json 的非 dict options 必须报错");
    assert!(e.contains("options 期望 dict"), "实得: {e}");
}

/// D160 反向对照：合法用法必须**逐字**不变（省略 / 空 dict / 带字段）。
#[test]
fn d160_compress_dict_options_still_work() {
    // 省略 options
    let a = run("compress(\"abcdefgh\", \"head_tail\")\n").expect("省略 options 应可用");
    assert!(
        a.contains("abcdefgh"),
        "省略 options 的行为不得变; 实得: {a}"
    );

    // 空 dict
    let b = run("compress(\"abcdefgh\", \"head_tail\", {})\n").expect("空 dict 应可用");
    assert_eq!(a, b, "空 dict 与省略 options 应等价");

    // 带字段的 dict（真正触发压缩）
    //
    // v0.104.6 D227：判据重写。原版是 `{max_bytes: 1}` + `contains("elided")`
    // + `starts_with('a') && ends_with('h')`，三处都与新契约冲突：
    // ① 注释自己已写明「压缩结果**可能比原文更长**（实测 8 字节 → 52 字符）」，
    //    这正是 D227 消除的现象 —— 现在输出必须 ≤ max_bytes；
    // ② elided marker 约 40 字节，max_bytes=1 时收口函数必须截掉它；
    // ③ 预算为 0 时 head/tail 两段都没了，保留首尾无从谈起。
    //
    // 「options 字段真的生效」的可测形态改成**输出被 max_bytes 约束**：
    // 输入 200 字节、上限 50，无 options 时原样 200 字节返回（默认 8192），
    // 有 options 时必须 ≤ 50。两条路径都必须如此。
    let long = "x".repeat(200);
    let c = run(&format!(
        "compress(\"{long}\", \"head_tail\", {{max_bytes: 50}})\n"
    ))
    .expect("带字段的 dict 应可用");
    assert_ne!(
        c, a,
        "max_bytes: 50 必须真的改变结果（这是 D149 钉住的路径）"
    );
    assert!(
        c.len() <= 50,
        "D227: 输出不得超过 max_bytes; 实得 {} 字节: {c}",
        c.len()
    );
    assert!(
        c.len() < long.len(),
        "必须真的截断; 实得 {} 字节 = 未截断",
        c.len()
    );

    // `json.parse` 产出的 dict（D149 钉住的 Int 路径）仍要生效
    let d = run(&format!(
        "let o = json.parse(\"{{\\\"max_bytes\\\": 50}}\")\ncompress(\"{long}\", \"head_tail\", o)\n"
    ))
    .expect("json.parse 出的 dict 应可用");
    assert_eq!(
        d, c,
        "`json.parse` 产出的 Int 值 dict 必须与字面量等价（D149 的路径不得回退）"
    );
}

/// D160 反向对照：`nil` 按「没传」处理（D150 起统一的约定）。
#[test]
fn d160_nil_options_means_absent() {
    let absent = run("compress(\"abcdefgh\", \"head_tail\")\n").expect("省略 options");
    let nil = run("compress(\"abcdefgh\", \"head_tail\", nil)\n")
        .expect("nil 是 Mora 的 null，可选实参传 nil 应视为没传");
    assert_eq!(nil, absent, "`nil` 与省略 options 必须等价");
}

/// D160 对照组：同函数里**本来就严格**的 `strategy` 形参不得回退。
#[test]
fn d160_strategy_argument_still_checked() {
    let e = run("compress(\"abcdefgh\", 5)\n").expect_err("strategy 传数字必须报错");
    assert!(
        e.contains("strategy must be a string"),
        "`compress` 的 strategy 形参必须仍然报错（本轮的正面样板）; 实得: {e}"
    );
}

/// D160 对照组：顶层 `crush_json(input, max)` 的 `max` 是**数字**形参，
/// D153 已收紧 —— 不得因本轮改动被误伤。
#[test]
fn d160_crush_json_max_still_requires_number() {
    let e = run("let xs = [1, 2, 3]\ncrush_json(xs, \"two\")\n")
        .expect_err("crush_json 的 max 传字符串必须报错");
    assert!(
        e.contains("must be a number"),
        "`crush_json` 的 max 形参必须仍然要求数字（D153 不得回退）; 实得: {e}"
    );
}

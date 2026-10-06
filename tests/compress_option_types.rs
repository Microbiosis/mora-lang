//! v0.104.6 D149：`compress` 的 options **静默吞掉两类输入**（已修）。
//!
//! ## 缺陷一：字段类型错 → 静默忽略
//!
//! ```mora
//! compress("abcdefgh", "head_tail", {max_bytes: "big"})
//! ```
//! 修复前 **exit 0、原样返回** —— 用户设了上限却毫无作用，且无任何提示。
//! 这与 D39（`with temperature = "hot"` 让配置**静默失效**）**同型**：
//! 同一族加固在别处做过，这里漏了。
//!
//! ## 缺陷二：**合法**输入被丢弃（更隐蔽）
//!
//! ```mora
//! let o = json.parse("{\"max_bytes\": 1}")
//! compress("abcdefgh", "head_tail", o)
//! ```
//! 修复前同样 **exit 0、原样返回**。原因：代码只匹配 `Value::Float`，
//! 而 dict **字面量**里的数字是 `Float`（D98）、`json.parse` 产出的是 `Int`（D129）——
//! 于是**完全合法的数字**被静默丢弃。
//!
//! 这正是 D148 的教训在另一层的体现：`Int` 与 `Float` 两侧都要管。
//!
//! ## 顺带记档：`max_bytes` 是**触发阈值**而非**硬上限**
//!
//! `compress/text.rs` 的注释明写「`max_bytes` **仅用于判断"是否需要压缩"**」：
//! 内容超过它就触发 head/tail 截断，但**结果本身可以超过该值**。
//! 名字容易让用户误解成硬上限。**未改动**，仅记档。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D149 主判据 ①：字段**类型错**必须报错（不再静默忽略）。
#[test]
fn d149_wrong_option_type_is_rejected_not_ignored() {
    for (key, bad) in [
        ("max_bytes", "\"big\""),
        ("target_ratio", "\"half\""),
        ("head_pct", "\"pct\""),
        ("preserve_errors", "1"), // 数值给了布尔位
        ("strategy", "5"),
        ("output_format", "5"),
    ] {
        let src = format!("compress(\"abcdefgh\", \"head_tail\", {{{key}: {bad}}})\nprint(1)\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "[{key}: {bad}] 类型错必须**报错**（修复前静默忽略、exit 0）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains(key),
            "[{key}] 错误信息应点名是哪个字段坏了"
        );
    }
}

/// D149 主判据 ②：**合法**的 `Int` 数字必须生效（修复前被静默丢弃）。
#[test]
fn d149_int_valued_options_from_json_parse_are_honoured() {
    // ⚠ 观测口径：`run()` 返回**末表达式**的值。写成 `print(compress(…))`
    //   时末值是 `print` 的 `Nil`（D107 同款陷阱）。故把结果**绑定到变量**
    //   再取末值，末值才是 compress 的产物。
    // `json.parse` 产出 `Int`（D129），修复前被 `if let Value::Float` 落空丢弃
    //
    // v0.104.6 D227：判据的观测方式必须重写。原版是
    // `{max_bytes: 1}` + `assert!(out.contains("elided"))`，现在**不可能**成立：
    // ① D227 的字节上限契约要求输出 ≤ max_bytes，而 elided marker 本身约
    //    40 字节，max_bytes=1 时收口函数必须把它截掉；
    // ② 即使放大到 12 字节，8 字节的输入仍装得下，**不发生省略**，
    //    也就没有 elided marker（实测输出 `abcdefgh` 原样返回）。
    //
    // 「Int 选项生效」的可测形态是**输出长度受 max_bytes 约束**，而不是
    // marker 文本。输入加长到 200 字节、上限设 50，两条路径（Int / Float）
    // 都必须截断到 ≤ 50 —— 修前 Int 路径被丢弃、默认 8192 会原样返回
    // 200 字节，判据立刻变红。
    let long = "x".repeat(200);
    let int_out = run(&format!(
        "let o = json.parse(\"{{\\\"max_bytes\\\": 50}}\")\nlet r = compress(\"{long}\", \"head_tail\", o)\nr\n"
    ))
    .unwrap_or_else(|e| panic!("合法的 Int 选项不应报错: {e}"));
    assert!(
        int_out.len() <= 50,
        "max_bytes: 50 (Int) 应**生效**并截断输出; 实得 {} 字节: {int_out}",
        int_out.len()
    );
    assert!(
        int_out.len() < long.len(),
        "Int 路径必须真的截断; 实得 {} 字节 = 未截断",
        int_out.len()
    );

    // 反向对照：同样的 Float（dict 字面量）本来就生效，两条路径现在一致
    let lit = run(&format!(
        "let r = compress(\"{long}\", \"head_tail\", {{max_bytes: 50}})\nr\n"
    ))
    .unwrap_or_else(|e| panic!("字面量 Float 选项不应报错: {e}"));
    assert!(
        lit.len() <= 50,
        "dict 字面量的 Float 选项必须照常生效并截断; 实得 {} 字节",
        lit.len()
    );
    assert_eq!(
        int_out, lit,
        "Int 与 Float 两条取值路径必须产出相同结果（D149 的核心主张）"
    );
}

/// 对照组：字段**缺失**仍合法跳过（options 是可选参数，不能改成必填）。
#[test]
fn d149_absent_options_are_still_fine() {
    for (name, src) in [
        ("no_opts", "print(compress(\"abcdefgh\", \"head_tail\"))\n"),
        (
            "empty_opts",
            "print(compress(\"abcdefgh\", \"head_tail\", {}))\n",
        ),
        (
            "only_strategy",
            "print(compress(\"abcdefgh\", \"head_tail\", {strategy: \"head_tail\"}))\n",
        ),
    ] {
        assert!(
            run(src).is_ok(),
            "[{name}] options 字段**缺失**必须仍然合法（可选参数）"
        );
    }
}

/// 对照组：D145 / D146 的负数拒绝**不得回退**（它们是本文件的前两道防线）。
#[test]
fn d149_negative_value_guards_still_hold() {
    for (key, src) in [
        (
            "max_bytes",
            "compress(\"abcdefgh\", \"head_tail\", {max_bytes: -1})\n",
        ),
        (
            "k_first",
            "compress(\"abcdefgh\", \"head_tail\", {k_first: -1})\n",
        ),
        (
            "k_last",
            "compress(\"abcdefgh\", \"head_tail\", {k_last: -1})\n",
        ),
    ] {
        let res = run(src);
        assert!(
            res.is_err(),
            "[{key}: -1] 的负数守卫不得回退（D145 / D146）"
        );
    }
}

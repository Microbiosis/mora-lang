//! v0.104.6 D145：`compress` 的 `max_bytes` 负数被**饱和转换**成 0（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! compress("abcdefgh", "head_tail", {max_bytes: -5})
//! ```
//!
//! 修复前与 `{max_bytes: 0}` 输出**逐字节相同** —— 8 字节输入里**只保留
//! head/tail 各 15%**（`a … [6 bytes elided] … h`），exit 0、零诊断。
//!
//! **根因**：`opts.max_bytes = Some(*n as usize)` —— Rust 的 float→int `as`
//! 是**饱和转换**，`-5.0 as usize == 0`。于是「上限写错符号」表现为
//! 「内容被悄悄删掉」，而不是报错。
//!
//! ## 为什么这条特别隐蔽
//!
//! 其它同族问题（`linalg.dot` 维度、`random.rand_int` 区间）都是**结果错**；
//! 这一条是**内容被删** —— 用户拿到的是一段「看起来像压缩结果」的文本，
//! 完全看不出数据丢了。
//!
//! ## 范围
//!
//! 只收紧**负数**。`max_bytes: 0` 的语义本身模糊（是「什么都不保留」还是
//! 「不限制」？），当前行为是「保留 head/tail 各 15%」；**未改动**，记档。

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

/// D145 主判据：负数 `max_bytes` 必须**报错**，不得静默删内容。
#[test]
fn d145_negative_max_bytes_is_rejected() {
    for n in ["-5", "-0.5", "-1"] {
        let src = format!("compress(\"abcdefgh\", \"head_tail\", {{max_bytes: {n}}})\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "max_bytes: {n} 必须报错（修复前被饱和转换成 0，内容被悄悄删光）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains("不能为负数"),
            "错误信息应点明「不能为负数」"
        );
    }
}

/// 反向对照：正常的 `max_bytes` 必须**逐字节不变**。
#[test]
fn d145_normal_max_bytes_is_unchanged() {
    // 上限足够 → 原样返回
    assert_eq!(
        run("compress(\"abcdefgh\", \"head_tail\", {max_bytes: 99999})\n").unwrap(),
        "abcdefgh"
    );
    // 上限等于内容长度 → 原样返回（`total <= max_bytes` 分支）
    assert_eq!(
        run("compress(\"abcdefgh\", \"head_tail\", {max_bytes: 8})\n").unwrap(),
        "abcdefgh"
    );
    // 上限 1 → 仍走压缩路径，保留首尾
    let one = run("compress(\"x\", \"head_tail\", {max_bytes: 1})\n").unwrap();
    assert_eq!(one, "x", "max_bytes: 1 对单字符内容应原样返回");
    // 不传 max_bytes → 用默认值 8192，8 字节内容原样返回
    assert_eq!(
        run("compress(\"abcdefgh\", \"head_tail\")\n").unwrap(),
        "abcdefgh"
    );
}

/// 对照组：未知策略**本就**明确报错（钉住防回退）。
#[test]
fn d145_unknown_strategy_still_rejected() {
    let res = run("compress(\"abc\", \"unknown_strategy\")\n");
    assert!(res.is_err(), "未知策略必须报错");
    assert!(
        res.unwrap_err().contains("unknown strategy"),
        "错误应点明 unknown strategy"
    );
}

//! v0.104.6 D129：运行时边界普查 —— 索引 / Unicode / JSON。
//!
//! 本轮从**编译期**（前 5 轮）转向**运行时**。结果以**否定结果**为主：
//! 三类边界各查一批，绝大部分健全；**唯一**的问题集中在 JSON 的大整数精度。
//!
//! ## 一、索引边界：全部健全
//!
//! | 场景 | 实测 |
//! |---|---|
//! | `xs[0]` 正常 | `1.0` ✓ |
//! | `xs[5]` 越界（len 2） | `index 5 out of bounds (len 2)`，exit 1 ✓ |
//! | `[] [0]` 空列表 | `index 0 out of bounds (len 0)`，exit 1 ✓ |
//! | `xs[-1]` 负索引 | `negative index: -1`，exit 1 ✓ |
//! | `[[1,2],[3]][0][1]` 嵌套 | `2.0` ✓ |
//! | `d["zzz"]` dict 缺键 | `nil`，继续执行（`get` 语义）✓
//!
//! **全部明确报错，无一处静默**（dict 缺键返回 `nil` 是 `get` 的既定语义）。
//!
//! ## 二、Unicode：**按 code point 而非字节**，包括最易出错的代理对
//!
//! 若字符串操作走字节切片，遇到多字节字符会 panic 或产生乱码。实测：
//!
//! | 源码 | 实测 | 说明 |
//! |---|---|---|
//! | `len("日本語")` | `3` | 字符数（字节数是 9） |
//! | `"日本語"[1]` | `本` | 字符索引 ✓ |
//! | `len("𝕏")` / `("𝕏")[0]` | `1` / `𝕏` | **代理对**（4 字节）；字节切片会 panic |
//! | `len("👨‍👩‍👧")` | `5` | ZWJ 组合 = 5 个 code point |
//! | `"a日b".split("日")` | `[a, b]` | ✓ |

use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => Ok(format!("{v}")),
        Err(e) => Err(e.to_string()),
    }
}

// ── 一、索引边界 ──────────────────────────────────────────────

#[test]
fn d129_index_bounds_are_all_reported_not_silent() {
    for (name, src, must_contain) in [
        ("list_oob", "let xs = [1, 2]\nxs[5]\n", "out of bounds"),
        ("empty_list_oob", "let xs = []\nxs[0]\n", "out of bounds"),
        (
            "negative_index",
            "let xs = [1, 2]\nxs[-1]\n",
            "negative index",
        ),
    ] {
        let res = run(src);
        assert!(
            res.is_err(),
            "[{name}] 越界必须**报错**（静默给默认值 = 数据静默损坏）; 实际: {res:?}"
        );
        let e = res.unwrap_err();
        assert!(
            e.contains(must_contain),
            "[{name}] 错误信息应含「{must_contain}」; 实际: {e}"
        );
    }
}

/// 对照组：合法索引正常，dict 缺键返回 `nil`（`get` 语义）。
#[test]
fn d129_valid_index_and_dict_get_still_work() {
    assert_eq!(run("let xs = [1, 2]\nxs[0]\n").unwrap(), "1.0");
    assert_eq!(run("let xs = [[1, 2], [3]]\nxs[0][1]\n").unwrap(), "2.0");
    // dict 缺键 → nil，且**不中断**执行
    assert_eq!(run("let d = {a: 1}\nd[\"zzz\"]\n").unwrap(), "nil");
}

// ── 二、Unicode：按 code point 而非字节 ───────────────────────

#[test]
fn d129_unicode_indexing_is_by_codepoint_not_byte() {
    // 字符索引：`"日本語"[1]` 是 `本`（按字节切会得到乱码或 panic）
    assert_eq!(run("\"日本語\"[1]\n").unwrap(), "本");
    assert_eq!(run("\"日本語\"[2]\n").unwrap(), "語");
    // len 按 code point 计数（字节数是 9）
    assert_eq!(run("len(\"日本語\")\n").unwrap(), "3");
}

#[test]
fn d129_surrogate_pair_and_zwj_are_handled() {
    // 代理对 `𝕏`：4 字节 / 1 code point。字节切片 `&s[0..1]` 会 panic。
    assert_eq!(run("len(\"𝕏\")\n").unwrap(), "1");
    assert_eq!(run("(\"𝕏\")[0]\n").unwrap(), "𝕏");
    // ZWJ 组合 `👨‍👩‍👧` = 5 个 code point（3 emoji + 2 个 ZWJ）
    assert_eq!(run("len(\"👨‍👩‍👧\")\n").unwrap(), "5");
}

#[test]
fn d129_unicode_split_is_correct() {
    assert_eq!(run("\"a日b\".split(\"日\")\n").unwrap(), "[a, b]");
    assert_eq!(run("len(\"a日b\")\n").unwrap(), "3");
}

// ── 三、JSON 边界：健全，但**大整数静默丢精度** ────────────────

#[test]
fn d129_json_parsing_edges_are_all_sound() {
    // 基本类型 + null → nil
    assert_eq!(
        run("let a = json.parse(\"{\\\"n\\\": 1.5, \\\"z\\\": null}\")\na\n").unwrap(),
        "{n: 1.5, z: nil}"
    );
    // 深层嵌套
    assert_eq!(
        run("let b = json.parse(\"{\\\"d\\\": {\\\"x\\\": 1}}\")\nb[\"d\"][\"x\"]\n").unwrap(),
        "1"
    );
    // 空 dict / 空 list / 顶层数组
    // ⚠ 元素是 `int` 而非 float —— JSON 的 i64 范围内整数走 `Value::Int`，
    //   只有超出 i64 才降级为 f64（见本文件最后一条）。
    assert_eq!(run("json.parse(\"{}\")\n").unwrap(), "{}");
    assert_eq!(run("json.parse(\"[]\")\n").unwrap(), "[]");
    assert_eq!(run("json.parse(\"[1, 2, 3]\")\n").unwrap(), "[1, 2, 3]");
    // 转义序列
    assert_eq!(
        run("len(json.parse(\"{\\\"e\\\": \\\"a\\\\\\\"b\\\\\\\\c\\\\nd\\\"}\")[\"e\"])\n")
            .unwrap(),
        "7"
    );
    // 重复键：后者覆盖前者（标准 JSON 行为）
    assert_eq!(
        run("json.parse(\"{\\\"x\\\": 1, \\\"x\\\": 2}\")[\"x\"]\n").unwrap(),
        "2"
    );
    // 非法 JSON / 尾逗号 → 明确报错
    assert!(run("json.parse(\"not json at all\")\n").is_err());
    assert!(run("json.parse(\"{\\\"k\\\": 1, }\")\n").is_err());
}

/// v0.104.6 **D197 已修**：JSON 整数超出 `i64` 范围时不再**静默**降级为 f64。
///
/// **历史**：D129 记下这个缺口时它是「唯一真问题」—— 降级点精确到
/// `i64::MAX`（约 19.3 位十进制），不是「约 17 位有效数字」（那只是降级
/// **之后** f64 自身的限制）。`i64` 以内的 JSON 整数一直是**精确**的。
/// 当时按「未实现的能力缺口」原则**只记录不实施**。
///
/// **现状**：`flow/json.rs::parse_json_number` 的整数路径改为
/// `i64` → **`BigInt`**（num-bigint 后端，真任意精度）。
/// 本语言早就有 `BigInt`（字面量 `<digits>n`，v0.91），只是 JSON 数字
/// 没映射过去 —— 于是把**已有的能力**接上，而不是新增能力。
///
/// D129 当时的测试断言「降级为 f64」作为**当前基线**，并留下注记：
/// 「若本测试失败，说明解析器已改为大整数感知」—— 正是本文件现在的状态，
/// 故按该注记翻转断言。
#[test]
fn d197_json_large_integer_is_exact_via_bigint() {
    // i64 以内**精确**（type 是 int 不是 float）
    assert_eq!(run("json.parse(\"5\")\n").unwrap(), "5");
    assert_eq!(
        run("json.parse(\"9223372036854775807\")\n").unwrap(),
        "9223372036854775807"
    );

    // i64::MAX + 1 —— 修前是 `9223372036854775808.0`（碰巧数值相同、类型已错），
    // 现在是 BigInt（`Display` 带 `n` 后缀，与 bigint 字面量一致）。
    assert_eq!(
        run("json.parse(\"9223372036854775808\")\n").unwrap(),
        "9223372036854775808n",
        "超出 i64 应得到 BigInt（带 n 后缀），而不是静默降级为 f64"
    );

    // **真正会失真**的情形（远超 f64 的 17 位有效数字）：
    // 修前得到 `123456789012345677877719597056.0` —— 与原值差 5 位有效数字，
    // 且**零提示**。现在必须逐位还原。
    assert_eq!(
        run("json.parse(\"123456789012345678901234567890\")\n").unwrap(),
        "123456789012345678901234567890n",
        "超长整数必须**逐位**保留 —— 修前静默失真"
    );

    // u64::MAX 这类「只差 1」的边界最能说明静默损坏的危害。
    assert_eq!(
        run("json.parse(\"18446744073709551615\")\n").unwrap(),
        "18446744073709551615n",
        "u64::MAX 修前变成 ...616（差 1）"
    );

    // 对照组：显式的 bigint 字面量行为不变（任意精度精确，`Display` 带 `n`）。
    assert_eq!(
        run("123456789012345678901234567890n\n").unwrap(),
        "123456789012345678901234567890n"
    );
    // 对照组：浮点数仍是 Float，**不被**误升为 BigInt。
    assert_eq!(run("json.parse(\"1.5\")\n").unwrap(), "1.5");
}

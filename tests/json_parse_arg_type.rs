//! v0.104.6 D155：`json.parse` 把**任何** Value 静默字符串化后当 JSON 解析（已修）。
//!
//! ## 缺陷
//!
//! ```rust
//! let text = args.first().map(|v| v.to_string()).unwrap_or_default();
//! json_to_value(&text)
//! ```
//!
//! 数字与布尔量的字符串形式**本身就是合法 JSON**，于是：
//!
//! | 调用 | 修复前 | 用户以为 |
//! |---|---|---|
//! | `json.parse(5)` | **5.0**，exit 0，零诊断 | 在解析一段 JSON 文本 |
//! | `json.parse(true)` | **true**，exit 0，零诊断 | 同上 |
//!
//! ## 为什么**不能**靠 typeck 拦
//!
//! `typeck/dispatch.rs` 给 `json.parse` 的签名是 `params_variadic(1, Type::Any)`
//! —— 那个 `Type::Any` 是**返回类型**（解析结果由文本决定，如实声明只能是 Any），
//! 而 `params()` 把**模块方法的所有形参一律声明为 `Type::Any`**
//! （保守约定，见该函数上方「宁可继续返回 TypeVar，也不要写一个没核对过的签名」）。
//! `Any` 与任何类型都能合一 → typeck 对模块实参**结构上无从检查**。
//!
//! 实测对照（同一函数，真实 CLI `mora --check`）：
//!
//! ```text
//! let xs = [1, 2, 3]  +  xs.take("one")   → 2 errors（值方法：形参声明为 Union(Int,Float)）
//! let a = math.floor("o")                 → No type errors found（模块方法：形参是 Any）
//! ```
//!
//! **模块方法与值方法的实参检查口径不同** —— 这是既有设计（D130 保守约定），
//! 本轮不动，只把它量化出来。故修复只能在**运行期**。
//!
//! 与同族一致：`document.parse` 的 path 同样要求字符串（D153 已修）。

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

/// D155 主判据 ①：非字符串实参必须**报错**，不得被静默字符串化后解析。
#[test]
fn d155_json_parse_rejects_non_string_argument() {
    for (arg, ty) in [("5", "float"), ("true", "bool"), ("[1,2]", "list")] {
        let res = run(&format!("json.parse({arg})\n"));
        let e = res.expect_err(&format!("json.parse({arg}) 传非字符串必须报错"));
        assert!(
            e.contains("text must be a string"),
            "[json.parse({arg})] 应报「text 必须是字符串」; 实得: {e}"
        );
        assert!(
            e.contains(ty),
            "[json.parse({arg})] 错误信息应点名实际类型 `{ty}`; 实得: {e}"
        );
    }
}

/// D155 主判据 ②：**缺参**必须报错，不得拿空串去解析。
///
/// 修复前是 `unwrap_or_default()` → `""` → `json_to_value("")`。
#[test]
fn d155_json_parse_requires_the_argument() {
    let e = run("json.parse()\n").expect_err("json.parse() 缺参必须报错");
    assert!(
        e.contains("requires text"),
        "缺参应报 requires text; 实得: {e}"
    );
}

/// D155 反向对照：合法用法必须**逐字**不变。
#[test]
fn d155_json_parse_string_paths_unchanged() {
    // ⚠ `json.parse` 的整数产出是 **`Int`**（D129 实测），故显示为 `1` 而非 `1.0`
    assert_eq!(run("json.parse(\"[1,2,3]\")\n").unwrap(), "[1, 2, 3]");
    assert_eq!(run("json.parse(\"{\\\"a\\\":1}\")\n").unwrap(), "{a: 1}");
    assert_eq!(run("json.parse(\"\\\"hi\\\"\")\n").unwrap(), "hi");
    // 变量传入的字符串同样可用
    assert_eq!(run("let s = \"[7,8]\"\njson.parse(s)\n").unwrap(), "[7, 8]");
    // 无效 JSON 仍报解析错（不是类型错）
    let e = run("json.parse(\"{oops\")\n").expect_err("无效 JSON 应报错");
    assert!(
        e.contains("json.parse"),
        "解析失败应仍归因到 json.parse; 实得: {e}"
    );
}

/// D155 对照组：同族里**本就严格**的那处不得回退。
///
/// `document.parse` 的 path 自 D153 起要求字符串 —— 它是 `json.parse`
/// 的同族正面样板，钉住它以防「统一」时被改成宽松。
#[test]
fn d155_document_parse_still_requires_string() {
    let e = run("document.parse(5)\n").expect_err("document.parse 传数字应报错");
    assert!(
        e.contains("path must be a string"),
        "`document.parse` 必须仍然要求字符串路径; 实得: {e}"
    );
}

/// **前提**判据：模块方法形参一律 `Type::Any` —— 所以 `json.parse` 传数字
/// **过不了 typeck、只能靠运行期拦**。本文件前面那些运行期判据因此是**承重的**。
///
/// v0.104.6 D172 改写：此前本条断言的是**源码字面量**
/// `src.contains("\"parse\" => Some(params_variadic(1, Type::Any))")`，
/// 而 D172 把 `module_method_signature` 重构成表驱动后该臂变成
/// `Some(1) => Some(params_variadic(1, Type::Any))` —— 断言转红而**语义未变**。
/// 典型的 D171 教训：**靠源码文本成立的判据，会被无关的代码形状变化打断**。
/// 改为断言**行为**：typeck 确实放行 `json.parse(5)`（故运行期那道关是承重的）。
#[test]
fn d155_module_method_params_are_declared_any() {
    let src = "json.parse(5)\n";
    let (_f, w) = mora::cli::compile_and_opt(src, None).expect("编译");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&w);
    assert!(
        errs.is_empty(),
        "前提：模块方法形参声明为 `Type::Any`，故 `json.parse(5)` 应**过不了 typeck**；\
         本文件的运行期判据因此是承重的。若本条转红，说明 typeck 开始检查了形参 —— \
         请把上面那些运行期判据上移到类型层; 实得: {errs:?}"
    );
    // 且运行期**确实**拒绝它 —— 两者合起来才是「typeck 放行、运行期兜住」
    let e = run(src).expect_err("运行期必须拒绝非字符串");
    assert!(
        e.contains("text must be a string"),
        "运行期必须点名 `text` 必须是字符串; 实得: {e}"
    );
}

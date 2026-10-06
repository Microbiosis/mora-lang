//! v0.104.6 D127：`dict.set` 的形参类型与 spec 不符（假阳性）—— 已修。
//!
//! ## 缺陷
//!
//! spec §12 方法表明写：
//!
//! ```text
//! | `.set(key, val)` | `string, any -> dict` | 设置键值（返回新字典） |
//! ```
//!
//! 形参 `val` 是 **`any`**。但 `typeck/dispatch.rs` 把它绑成 **dict 自身的
//! `V`**，于是：
//!
//! ```mora
//! let d = {a: 1}
//! let e = d.set("b", "text")     // 修复前：expected float, got string
//! ```
//!
//! 而运行期 `Value::Dict(HashMap<String, Value>)` **完全支持**异质，
//! `json.parse` 产出的 dict 也天然是异质的（它的 `V` 未受约束，那条路径可用）。
//! 于是同一门语言里出现**两套行为**：「从字面量 dict 出发就不能再加异质键」。
//!
//! ## 判定：实现疏漏，不是设计决定
//!
//! - spec 明确写 `any`（实现与文档冲突 → 按 D108 / D109 / D116 / D117 的先例
//!   改代码）；
//! - `keys` / `values` / `len` / `get` 四条签名**都正确**，`set` 是**孤例**；
//! - 运行时本来就支持异质，typeck 的限制是凭空多出来的。
//!
//! ## 修法
//!
//! 形参 `value` 放宽为 `Type::Any`，返回类型同步放宽为 `Dict(k, Any)`
//! —— 若只放宽形参而仍返回 `Dict(k, v)`，存进去的 `Any` 与声明的 `v`
//! 会对不上，`e["b"]` 又会被按 `v` 复查而报错（实测会这样）。

use mora::cli::compile_and_opt;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use mora::typeck::format_error;

fn diagnostics(src: &str) -> Vec<String> {
    let (_f, wits) = compile_and_opt(src, None).expect("应能编译");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(format_error)
        .collect()
}

/// 主判据：字面量 dict **可以**添加异质键（对齐 spec 的 `val: any`）。
#[test]
fn d127_dict_set_accepts_a_heterogeneous_value() {
    let errs = diagnostics("let d = {a: 1}\nlet e = d.set(\"b\", \"text\")\nprint(e)\n");
    assert!(
        errs.is_empty(),
        "`set` 的 val 形参按 spec 是 `any`，异质值必须被接受; 实际诊断: {errs:?}"
    );
}

/// 放宽形参后，返回的 dict 必须**真的**能索引到那个新键
/// ——返回类型若仍是 `Dict(k, v)`，这条会因 `e["b"]` 被按 `v` 复查而失败。
#[test]
fn d127_the_new_key_is_readable_from_the_returned_dict() {
    let errs = diagnostics(
        "let d = {a: 1}\nlet e = d.set(\"b\", \"text\")\nprint(e[\"b\"])\nprint(e[\"a\"])\n",
    );
    assert!(
        errs.is_empty(),
        "set 返回的 dict 必须能索引新旧两个键（返回类型须同步放宽为 Dict(k, Any)）; \
         实际诊断: {errs:?}"
    );
}

/// 对照组 ①：同质 value 仍可用（放宽不能把别的路径弄坏）。
#[test]
fn d127_same_typed_set_still_works() {
    for (name, src) in [
        (
            "number",
            "let d = {a: 1}\nlet e = d.set(\"b\", 2)\nprint(e[\"b\"])\n",
        ),
        (
            "string",
            "let d = {a: \"x\"}\nlet e = d.set(\"b\", \"y\")\nprint(e[\"b\"])\n",
        ),
        (
            "bool",
            "let d = {a: 1}\nlet e = d.set(\"b\", true)\nprint(e[\"b\"])\n",
        ),
    ] {
        let errs = diagnostics(src);
        assert!(
            errs.is_empty(),
            "[{name}] 同质 set 必须可用; 实际: {errs:?}"
        );
    }
}

/// 对照组 ②：`json.parse` 得到的异质 dict 路径**不得回退**
/// （修复前那条路径就是可用的，若此次修复反而弄坏它即为回归）。
#[test]
fn d127_json_parse_dict_set_still_works() {
    let errs = diagnostics(
        "let d = json.parse(\"{\\\"a\\\": 1}\")\nlet e = d.set(\"b\", true)\nprint(e[\"b\"])\n",
    );
    assert!(
        errs.is_empty(),
        "json.parse 路径必须照旧可用; 实际: {errs:?}"
    );
}

/// 反向对照：**key 形参的类型约束必须保留**。
///
/// `val` 放宽成 `any` 之后，若连 `key` 也一起放宽成 `any`，
/// `d.set(5, "x")` 就会被静默接受 —— 而运行期 dict 的键是 `String`。
///
/// ⚠ 断言「**至少**一条」而非「恰好一条」：同一条 key 冲突目前会被报**两次**
/// （`set` 有两个形参，`infer_method_call` 为每个实参各推一个 `Eq` 约束，
/// 两个都撞在同一个 key 上）。这是**既有**的重复诊断缺陷 ——
/// 反向验证证实：`set` 签名改回旧样（`value: V`）时 `d.set(5, "x")` 报 **4 条**，
/// 改后是 2 条，即本修复顺带把它从 4 降到 2，**但没有根除**。
/// 根治（同一冲突只报一次）已记入 CHANGELOG D127 待办。
#[test]
fn d127_set_key_still_must_be_a_string() {
    let errs = diagnostics("let d = {a: 1}\nlet e = d.set(5, \"x\")\nprint(e)\n");
    assert!(
        !errs.is_empty(),
        "`set` 的 key 是数字必须仍被拒绝（spec 签名是 `string, any -> dict`）; 实际: {errs:?}"
    );
    assert!(
        errs.iter().all(|e| e.contains("string")),
        "诊断应点明 key 期望 string; 实际: {errs:?}"
    );
}

/// 反向对照：dict 字面量的**同质约束**本身不受影响（D124 的契约）。
///
/// 放宽的是 `set` 的形参，不是字面量 —— `{a: 1, b: "x"}` 仍须被拒。
#[test]
fn d127_dict_literal_homogeneity_is_unaffected() {
    let errs = diagnostics("let d = {a: 1, b: \"x\"}\nprint(d)\n");
    assert_eq!(
        errs.len(),
        1,
        "异质 dict 字面量仍须被拒（D124 契约）; 实际: {errs:?}"
    );
    assert!(
        errs[0].contains("同质"),
        "仍应报 D124 的同质诊断; 实际: {}",
        errs[0]
    );
}

/// 回归钉住：`keys` / `values` / `len` / `get` 四条签名正确，**不得**被此次
/// 改动牵连（`set` 是孤例，其余四条本来就对）。
#[test]
fn d127_neighbouring_dict_signatures_are_untouched() {
    let src = "let d = {a: 1, b: 2}\n\
               let ks = d.keys()\n\
               let vs = d.values()\n\
               let n: number = d.len()\n\
               let g = d.get(\"a\")\n\
               print(ks, vs, n, g)\n";
    let (_f, wits) = ParserV3::compile(src).expect("语法应通过");
    let errs = check_program_witnesses_bidirectional(&wits);
    assert!(
        errs.is_empty(),
        "keys/values/len/get 的签名不受 `set` 改动影响; 实际: {:?}",
        errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
}

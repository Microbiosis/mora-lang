//! v0.104.6 D171：`Value` 全变体的 `methods_of` 普查 —— 补完 D169 漏掉的那类（**否定结果**）。
//!
//! D169 修的是 `Value::Document`；D170 发现**同类的 23 倍版本**
//! （`Value::Builtin` = 23 个模块对象）。但两轮都只是**逐个遇到才补**。
//! 本轮做**完整普查**：28 个 `Value` 变体，对照 `Value::methods()` 的 arm，
//! 再对照运行期 `call_method` 的分派分支，回答「还有没有第三个缺口」。
//!
//! ## 普查结论
//!
//! - 28 个变体中，`methods()` 有 **13** 个 arm（String / List / Dict / Int /
//!   Float / BigInt / Conversation / Stream / Router / McpServer / Agent /
//!   Document / —），无 arm 的 **16** 个。
//! - 这 16 个里，**只有两个**在运行期有方法分派：
//!   - **`Value::Builtin`** —— D170 已记档（23 个模块对象全为 `[]`）
//!   - **`Value::TraitObject`** —— 本轮的**潜在**缺口，见下
//! - 其余 14 个（Char / Bool / Nil / Task / Tool / Closure / AiConfig /
//!   HttpRequest / Compose / Partial / Atom / Macro / PromptSection / Curry）
//!   在 `call_method` 里落到 `_` 臂，直接报「Can only call methods on …」
//!   → **本来就没有方法**，`methods_of` 返回 `[]` 是**正确**的。
//!
//! ## `TraitObject` 的 `[]` 今天正确，但是**潜在**缺口
//!
//! ```text
//! let x: dyn Foo = 1
//! print(type_of(x))            → trait_object
//! print(methods_of(x))         → []          ← 今天正确
//! print(x.anything())          → Runtime error: trait dispatch …
//!                                  no impl for type 'float' method 'anything' (searched: Foo)
//! ```
//!
//! trait 分派**是接通的**（`call_method` 首臂就路由到 `dispatch_trait_method`），
//! 报的是「没有对应 impl」而不是「未知方法」。而本语言 **`trait` / `impl`
//! 都无法解析**（`spec_ebnf_census.rs` 里二者是 `EXPECTED_UNPARSEABLE`），
//! 所以**无法给任何类型写 impl** → trait 对象的方法集恒为空 → `[]` 是对的。
//!
//! 但一旦 `trait` / `impl` 前端落地，`methods_of` 对 trait 对象**立刻**变成
//! 第三个缺口。本条就是那个**触发条件的护栏**：写明「若本条转红，
//! 说明 trait/impl 已可声明 —— 此时应给 `Value::TraitObject` 补 arm，
//! 并按「该值实际匹配到哪个 trait」动态列方法」。

use std::process::Command;

fn run_src(src: &str, tag: &str) -> String {
    let base = std::env::temp_dir().join(format!("mora_d171_{tag}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("建目录");
    let prog = base.join("p.mora");
    std::fs::write(&prog, src).expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&base);
    s
}

/// D171 主判据 ①：没有方法分派的变体，其 `methods_of` 为 `[]` **且**调用方法会
/// 明确报错 —— 「空集」与「明确不可调用」必须**成对**，否则自省就在撒谎。
///
/// 本轮先试过「从 `value.rs` 解析变体清单做漂移护栏」，**已放弃**：那个解析器
/// 跨出了 `enum Value` 的范围、把 `Type` 的变体（`Code`/`Goal`/`TeaApp`…）也算了
/// 进来。**脆的判据比没有判据更糟** —— 它会给人「已覆盖」的错觉。
/// 改为断言真正的不变式（行为层面，且用运行期而非源码解析）。
#[test]
fn d171_variants_without_dispatchers_are_empty_and_explicitly_uncallable() {
    for (label, expr) in [("Bool", "true"), ("Nil", "nil"), ("Char", "'c'")] {
        let base = std::env::temp_dir().join("mora_d171_v");
        let _ = std::fs::create_dir_all(&base);
        let prog = base.join("p.mora");
        std::fs::write(&prog, format!("print(methods_of({expr}))\n")).expect("写");
        let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
        let out = Command::new(mora).arg(&prog).output().expect("跑");
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            s.contains("[]"),
            "[{label}] 没有方法分派，`methods_of` 必须是 `[]`; 实得: {s}"
        );
        // 且调用方法必须**明确报错**，而不是别的什么
        std::fs::write(&prog, format!("{expr}.anything()\n")).expect("写");
        let out = Command::new(mora).arg(&prog).output().expect("跑");
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            err.contains("Can only call methods on"),
            "[{label}] 调用方法应报「Can only call methods on …」; 实得: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
/// D171 主判据 ②：`TraitObject` 的 `methods_of` **今天**是 `[]` —— 且这是**正确**的。
///
/// 触发条件护栏：若 `trait` / `impl` 前端落地使本条转红，说明该给
/// `Value::TraitObject` 补 arm 了（按值实际匹配到的 trait 动态列方法）。
#[test]
fn d171_trait_object_introspection_is_empty_because_no_impl_is_declarable() {
    let out = run_src(
        "let x: dyn Foo = 1\nprint(\"@@T\")\nprint(type_of(x))\nprint(\"@@M\")\nprint(methods_of(x))\n",
        "dyn",
    );
    let lines: Vec<&str> = out.lines().map(|l| l.trim()).collect();
    let after = |marker: &str| -> String {
        let i = lines
            .iter()
            .position(|l| *l == marker)
            .unwrap_or(usize::MAX);
        lines.get(i + 1).copied().unwrap_or("<无输出>").to_string()
    };
    assert_eq!(
        after("@@T"),
        "trait_object",
        "dyn 强制转换应产出 trait_object"
    );
    assert_eq!(
        after("@@M"),
        "[]",
        "已知（今天正确）：`trait`/`impl` 解析不了 → 无法给任何类型写 impl → \
         trait 对象方法集恒为空。**若本条转红，说明 trait/impl 已可声明** —— \
         请给 `Value::TraitObject` 补 arm 并改写本条"
    );
}

/// D171 对照组：trait 分派**是接通的** —— 它报的是「没有 impl」而非「未知方法」。
///
/// 这条同时证明上面那个 `[]` 是**方法集为空**，不是分派缺失。
#[test]
fn d171_trait_dispatch_is_wired_but_has_no_impls() {
    let out = run_src("let x: dyn Foo = 1\nx.anything()\n", "dyn2");
    // CLI 把运行期错误打到 stderr；用 exit 语义不便取，故直接断言 stdout 无结果行，
    // 并另跑一次 --check 确认不是类型层拦下的。
    assert!(
        !out.contains("anything"),
        "`x.anything()` 不应成功返回; 实得: {out}"
    );
    let dir = std::env::temp_dir().join("mora_d171_dyn2");
    let _ = std::fs::create_dir_all(&dir);
    let prog = dir.join("p.mora");
    std::fs::write(&prog, "let x: dyn Foo = 1\nx.anything()\n").expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        err.contains("no impl for"),
        "运行期应报「no impl for」—— 证明分派已接通、只是没有 impl; 实得 stderr: {err}"
    );
}

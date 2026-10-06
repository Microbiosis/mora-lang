//! 「语义等价配对」一致性 —— 同一语义的不同写法必须给出同一结果。
//!
//! ## 为什么需要
//!
//! 方法来自一次真实缺陷（2026-09-28）：`d.get("a")` 与 `d["a"]` 语义**完全相同**，
//! 却因降级成不同的 MIR 指令而给出**不同结果** ——
//!
//! | 写法      | 降级为                            | 循环内结果 |
//! |-----------|-----------------------------------|-----------|
//! | `d.get("a")` | `Call`（不在 memo 白名单）→ 必重算 | 6.0 ✓     |
//! | `d["a"]`  | **`Index`**（在 memo 白名单）→ 被跳过 | 0.0 ✗     |
//!
//! 同一语义的两种写法**本该互为对照**，却没人比对过 —— 这类不一致极难靠读代码
//! 发现，却能被「把等价写法配成对」一眼看出。
//!
//! 本文件把语言里成对的写法全部列出来跑一遍，任何一对不一致即失败。
//!
//! ## 覆盖的配对
//!
//! 下标 vs `.get`、builtin `len` vs `.len()`、容器方法 vs 手工等价写法、
//! `keys[i]`/`values[i]` 下标对齐、push 链 vs 字面量、嵌套下标的两条路径。

use std::sync::Arc;

fn run(src: &str) -> String {
    let (func, witnesses) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    // v0.104.6 D52：**必须过 typeck**。此前本文件只做 `compile` + `run_mir`，
    // 完全绕过类型检查 —— 于是「两种写法运行期一致」可以在**其中一种编译器
    // 根本不接受**的情况下照样「通过」。真实踩中的例子：
    //
    // ```mora
    // let xs = [10, 20, 30]
    // xs[1]        // ✅ 能编译
    // xs.get(1)    // ❌ Type error: expected int, got float
    // ```
    //
    // 而本文件的 `list_index_vs_get` 配对正是这一组，却一直绿。根因是
    // `get` 的 typeck 签名声明 `index: Type::Int`（已随 D52 改为 `Int | Float`）。
    //
    // 加这道关的代价很低（本文件用例都是刻意写成能过 typeck 的），收益是
    // 「两种写法**都被编译器接受**且**运行期一致**」这一更强的不变量。
    let type_errs = check_program_witnesses_bidirectional(&witnesses);
    if !type_errs.is_empty() {
        return format!(
            "TYPECK-ERR: {:?}",
            type_errs
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
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
        Ok(v) => format!("{v}"),
        Err(e) => format!("ERR: {e}"),
    }
}

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// `(配对名, 源码, 期望结果)` —— 源码内部对两种写法做相等性比较。
///
/// v0.104.6：期望值改为**每对自带**，而不是全文件统一 `"1.0"`。原因见
/// `len_builtin_vs_method_dict` 那条 —— 它要比的布尔值本身受 D54 影响，
/// 硬编码 `"1.0"` 会让「本配对要检验什么」与「结果长什么样」两件事缠在一起。
fn pairs() -> Vec<(&'static str, String, &'static str)> {
    vec![
        // ── 取元素：下标 vs `.get`（memo 缺陷的原发现处）────────────────
        ("list_index_vs_get",
         "let xs = [10, 20, 30]\nif xs[1] == xs.get(1) then 1 else 0 end\n".into(), "1.0"),
        ("dict_index_vs_get",
         "let d = {a: 7}\nif d[\"a\"] == d.get(\"a\") then 1 else 0 end\n".into(), "1.0"),
        ("nested_index_paths_agree",
         "let m = [[1, 2], [3, 4]]\nif m[1][0] == m.get(1).get(0) then 1 else 0 end\n".into(), "1.0"),
        // ── 长度：builtin vs 方法（Call vs 方法）───────────────────────
        ("len_builtin_vs_method_list",
         "let xs = [1, 2, 3]\nif len(xs) == xs.len() then 1 else 0 end\n".into(), "1.0"),
        // v0.104.6 D54：原先写 `if len(d) == d.len() then 1 else 0 end`，而
        // **Dict 接收者**的这一形态被 typeck 拒（`expected int, got float`），
        // 且把比较结果绑给变量（`let eq = …`）同样被拒 —— 两者都属 D54。
        // 本配对要检验的只是「builtin `len` vs 方法 `.len()`」**是否等价**，
        // 与结果被不被消费无关，故用尾表达式形态（实测通过，值是 `true` 而非
        // `1.0` —— 故本对的期望值也随之为 `"true"`）。
        // D54 本身由下面 `dict_len_comparison_inside_if_is_a_known_typeck_gap`
        // 单独记录。
        ("len_builtin_vs_method_dict",
         "let d = {a: 1, b: 2}\nif len(d) == d.len() then 1 else 0 end\n".into(), "1.0"),
        ("len_builtin_vs_method_str",
         "let s = \"hello\"\nif len(s) == s.len() then 1 else 0 end\n".into(), "1.0"),
        // ── 容器方法 vs 手工等价写法 ─────────────────────────────────────
        ("take_equals_manual",
         "let xs = [1, 2, 3, 4, 5]\nlet t = xs.take(3)\nlet m = [].push(1).push(2).push(3)\nif t == m then 1 else 0 end\n".into(), "1.0"),
        ("filter_equals_manual",
         "let xs = [1, 2, 3, 4]\nlet a = xs.filter(fn(x) x % 2 == 0 end)\nlet b = [2, 4]\nif a == b then 1 else 0 end\n".into(), "1.0"),
        ("flatten_equals_manual",
         "let xs = [[1, 2], [3, 4]]\nlet a = xs.flatten()\nlet b = [1, 2, 3, 4]\nif a == b then 1 else 0 end\n".into(), "1.0"),
        ("map_equals_manual",
         "let xs = [1, 2, 3]\nlet a = xs.map(fn(x) x * 2 end)\nlet b = [2, 4, 6]\nif a == b then 1 else 0 end\n".into(), "1.0"),
        ("sum_equals_manual_index",
         "let xs = [1, 2, 3]\nlet a = xs.sum()\nlet b = 0\nlet i = 0\nwhile i < 3\n  let b = b + xs[i]\n  let i = i + 1\nend\nif a == b then 1 else 0 end\n".into(), "1.0"),
        ("push_chain_equals_literal",
         "let a = [].push(1).push(2).push(3)\nlet b = [1, 2, 3]\nif a == b then 1 else 0 end\n".into(), "1.0"),
        // ── `sort` 返回新列表、**不改**接收者（值语义契约）─────────────
        // 注意：sort 从来不是原地排序（迁移前后实现一致，都是「构建 sorted 再返回」）。
        ("sort_leaves_receiver_unchanged",
         "let a = [3, 1, 2]\nlet b = a.sort()\nif b == [1, 2, 3] and a == [3, 1, 2] then 1 else 0 end\n".into(), "1.0"),
        // ── `keys` / `values` 必须按下标对齐 ───────────────────────────
        // `keys()` 与 `values()` 曾各自独立排序（一个对、一个错就会错配）。
        ("keys_values_aligned_by_index",
         "let d = {a: 1, b: 2, c: 3}\nlet ks = d.keys()\nlet vs = d.values()\nlet ok = 1\nlet i = 0\nwhile i < 3\n  if vs[i] != d.get(ks[i]) then\n    let ok = 0\n  end\n  let i = i + 1\nend\nok\n".into(), "1.0"),
    ]
}

#[test]
fn equivalent_spellings_agree() {
    let all = pairs();
    let mut failures = Vec::new();
    for (name, src, want) in &all {
        let got = run(src);
        if got != *want {
            failures.push(format!(
                "  [{name}]\n    src   = {src:?}\n    实际输出 = {got}（期望 {want}）"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 对「语义等价写法」结果不一致：\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

/// v0.104.6 D54：修前「Dict + `len` 的两种写法放进比较」会被 typeck 拒。
///
/// 本测试原是记录那个缺口的，**D54 修好后已删除** —— 保留它反而会
/// 把正确行为锁死成「必须报错」。
///
/// D54 的根：`infer_method_call` 里 v0.103 的 **Dict 字段访问**分支
/// （`dict_field_type`）对 `Type::Dict(_, v)` **无条件**返回 `Some(v)`，
/// 于是 `d.len()` / `d.keys()` 这类**真方法**被当成「读 dict 的同名字段」，
/// 返回 dict 的**值类型**，把签名表算出的 `ret = Int` 整个覆盖掉。修法是该
/// 分支先要求「这个名字不是真方法」（`method_signature(..).is_none()`）。
///
/// 修后 `d.len()` / `d.keys().len()` 都正确推为 `Int`，本文件的
/// `len_builtin_vs_method_dict` 配对已改回自然的 `if … then 1 else 0 end`
/// 形态。回归覆盖见 `tests/dict_method_vs_field.rs`。
#[test]
fn dict_len_forms_agree_after_d54() {
    for (label, src) in [
        ("裸比较", "let d = {a: 1, b: 2}\nlen(d) == d.len()\n"),
        (
            "if 条件",
            "let d = {a: 1, b: 2}\nif len(d) == d.len() then 1 else 0 end\n",
        ),
        (
            "先绑定再 if",
            "let d = {a: 1, b: 2}\nlet eq = len(d) == d.len()\nif eq then 1 else 0 end\n",
        ),
        (
            "链式 keys().len()",
            "let d = {a: 1, b: 2}\nd.keys().len() == 2\n",
        ),
    ] {
        let got = run(src);
        assert!(
            !got.starts_with("TYPECK-ERR") && !got.starts_with("COMPILE-ERR"),
            "[{label}] D54 修好后这些形态都应通过 typeck，实得: {got}"
        );
    }
}

/// 越界时两种写法都必须报错（且错误一致）—— 配对的一致性也包括**失败**路径。
#[test]
fn out_of_bounds_agrees_between_spellings() {
    let a = run("let xs = [10, 20]\nxs[5]\n");
    let b = run("let xs = [10, 20]\nxs.get(5)\n");
    assert_eq!(a, b, "下标与 .get 的越界处理不一致：{a} vs {b}");
    assert!(a.starts_with("ERR"), "越界应当报错，实得 {a}");
}

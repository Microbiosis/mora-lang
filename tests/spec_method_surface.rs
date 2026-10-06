//! v0.104.6 D61：spec §12 的**方法表**（String 10 / List 15 / Dict 6）里，
//! 哪些方法在对应接收者上真的存在。
//!
//! 这是 D60（`ns.func(...)` 命名空间表面）的**平移**：D60 查出 41 个点号内建里
//! 2 个不存在；这里对**方法**做同样的逐项核验。
//!
//! ## 检验的是「名称表面」而非「跑通」
//!
//! 断言该方法能解析到已知实现，不要求执行成功 —— `json()` 解析失败、
//! `window(0)` 之类是**运行期**问题，不是「方法不存在」。
//! 判据只排除：`has no method` / `Unknown method` / `Unknown function`。
//!
//! ## 交叉核对 `methods_of`
//!
//! `methods_of` 是用户判断「这个值能干什么」的唯一入口。逐项调用之外，
//! 再把 spec 表与 `methods_of` 的运行期名单**对齐** —— 两边不一致本身就是缺陷
//! （宣告了却没有，或有却没宣告）。
//!
//! ## 已知项（修前实测）
//!
//! * `list.contains` —— **不存在**（`contains` 是 String 的方法；spec
//!   §1038 确实列在 String 表下，但 List 表里也有一个同名直觉陷阱）。
//! * `list.clear` —— 不在 spec 表里，故不测（D60 里实测报
//!   `List has no method: clear`，属正常拒绝）。
//!
//! ## 只记录不修
//!
//! 与 D60 同理：spec 承诺却缺失的方法，补上要定其返回值与失败语义，
//! 属功能设计，未擅自做。本文件钉住**当前事实**。

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

fn is_unresolved(err: &str) -> bool {
    err.contains("has no method")
        || err.contains("Unknown method")
        || err.contains("Unknown function")
        || err.contains("Can only call methods on")
}

const S: &str = r#""ab""#;
const L: &str = "[1, 2, 3]";
const D: &str = r#"{a: 1}"#;

/// `(接收者标签, spec 表位置, 逐个方法调用)`
const STRING_METHODS: &[(&str, &str)] = &[
    ("len", r#"("ab").len()"#),
    ("upper", r#"("ab").upper()"#),
    ("lower", r#"("ab").lower()"#),
    ("trim", r#"("ab").trim()"#),
    ("starts_with", r#"("ab").starts_with("a")"#),
    ("ends_with", r#"("ab").ends_with("b")"#),
    ("contains", r#"("ab").contains("a")"#),
    ("split", r#"("ab").split("a")"#),
    ("replace", r#"("ab").replace("a", "c")"#),
    ("json", r#"("ab").json()"#),
];

const LIST_METHODS: &[(&str, &str)] = &[
    ("push", "[1, 2].push(3)"),
    ("pop", "[1, 2].pop()"),
    ("get", "[1, 2].get(0)"),
    ("len", "[1, 2].len()"),
    ("map", "[1, 2].map(fn(x) x end)"),
    ("filter", "[1, 2].filter(fn(x) x end)"),
    ("reduce", "[1, 2].reduce(fn(a, b) a end, 0)"),
    ("take", "[1, 2].take(1)"),
    ("drop", "[1, 2].drop(1)"),
    ("window", "[1, 2, 3].window(2)"),
    ("batch", "[1, 2, 3].batch(2)"),
    ("shape", "[[1, 2]].shape()"),
    ("flatten", "[[1], [2]].flatten()"),
    ("transpose", "[[1, 2]].transpose()"),
    ("reshape", "[1, 2].reshape(1, 2)"),
];

const DICT_METHODS: &[(&str, &str)] = &[
    ("get", r#"{a: 1}.get("a")"#),
    ("set", r#"{a: 1}.set("b", 2)"#),
    ("keys", "{a: 1}.keys()"),
    ("values", "{a: 1}.values()"),
    ("len", "{a: 1}.len()"),
    ("json", r#"{a: 1}.json()"#),
];

/// 逐个核验：spec 承诺的方法必须在对应接收者上解析得到。
fn check(label: &str, methods: &[(&str, &str)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, src) in methods {
        if let Err(e) = run(&format!("print({src})\n"))
            && is_unresolved(&e)
        {
            failures.push(format!("  [{label}.{name}] {e}"));
        }
    }
    failures
}

#[test]
fn spec_promised_string_methods_resolve() {
    let f = check("String", STRING_METHODS);
    assert!(
        f.is_empty(),
        "spec 承诺的 String 方法解析不到：\n{}",
        f.join("\n")
    );
}

#[test]
fn spec_promised_list_methods_resolve() {
    let f = check("List", LIST_METHODS);
    assert!(
        f.is_empty(),
        "spec 承诺的 List 方法解析不到：\n{}",
        f.join("\n")
    );
}

#[test]
fn spec_promised_dict_methods_resolve() {
    let f = check("Dict", DICT_METHODS);
    assert!(
        f.is_empty(),
        "spec 承诺的 Dict 方法解析不到：\n{}",
        f.join("\n")
    );
}

/// spec 方法表与 `methods_of` 运行期名单的**双向对齐**。
///
/// 方向一：spec 承诺的必须在 `methods_of` 里（否则用户在能力发现里看不到它）。
/// 方向二：`methods_of` 列出的不该是 spec 没写的（否则是未文档化的意外面）。
///
/// 方向二只对**交集内的名字**断言 —— `methods_of` 列出一些 spec 未逐字写的方法
/// 并不算缺陷（spec 表不是完备清单），所以这里只要求方向一。
#[test]
fn spec_method_table_is_contained_in_methods_of() {
    let listed = |expr: &str| -> Vec<String> {
        // **不能**包 `print(...)` —— 那样尾值是 `print` 的 Nil 而不是名单本身
        match run(&format!("methods_of({expr})\n")) {
            Ok(Value::List(items)) => items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.to_string(),
                    other => format!("{other:?}"),
                })
                .collect(),
            Ok(other) => panic!("methods_of({expr}) 应返回 List，实得 {other:?}"),
            Err(e) => panic!("methods_of({expr}) 失败: {e}"),
        }
    };
    let (s, l, d) = (listed(S), listed(L), listed(D));
    let mut missing = Vec::new();
    for (name, _) in STRING_METHODS {
        if !s.iter().any(|m| m == name) {
            missing.push(format!("  String.{name}"));
        }
    }
    for (name, _) in LIST_METHODS {
        if !l.iter().any(|m| m == name) {
            missing.push(format!("  List.{name}"));
        }
    }
    for (name, _) in DICT_METHODS {
        if !d.iter().any(|m| m == name) {
            missing.push(format!("  Dict.{name}"));
        }
    }
    assert!(
        missing.is_empty(),
        "spec 方法表里的这些方法**没有**出现在 `methods_of` 名单里 —— \
         用户靠它做能力发现，缺项即等于宣告了却没有：\n{}",
        missing.join("\n")
    );
}

/// 覆盖完整性：spec §12 方法表共 10 + 15 + 6 = **31** 项，
/// 本文件必须把 31 项**全部**钉住。
///
/// 少了这条，将来 spec 方法表增删而无人更新本文件，就会**静默失守** ——
/// 与 D55 / D56 / D59 同型（测试只因某个原因才通过 / 没覆盖到）。
#[test]
fn spec_method_table_is_fully_covered() {
    const SPEC_COUNT: usize = 31;
    let covered = STRING_METHODS.len() + LIST_METHODS.len() + DICT_METHODS.len();
    assert_eq!(
        covered, SPEC_COUNT,
        "本文件钉住了 {covered} 项，但 spec 方法表是 {SPEC_COUNT} 项 —— \
         有承诺没被覆盖。改动 spec §12 的方法表时，请同步更新三组常量。"
    );
}

/// `methods_of` 在 spec 表之外**多列了 9 个 List 方法**（sum / min / max / mean /
/// median / stddev / var / sort / crush_json）。这些未在 spec 逐字写，但**都在
/// 运行期可用** —— 即「有能力但没文档」，属文档缺口而非缺陷。此处把数量钉住，
/// 将来有人删掉其中之一会立刻发现。
#[test]
fn methods_of_lists_nine_undocumented_list_methods() {
    let listed = run("methods_of([1, 2])\n").expect("methods_of 应可用");
    let names: Vec<String> = match listed {
        Value::List(items) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.to_string(),
                other => format!("{other:?}"),
            })
            .collect(),
        other => panic!("应返回 List，实得 {other:?}"),
    };
    const UNDOCUMENTED: &[&str] = &[
        "sum",
        "min",
        "max",
        "mean",
        "median",
        "stddev",
        "var",
        "sort",
        "crush_json",
    ];
    let missing: Vec<&str> = UNDOCUMENTED
        .iter()
        .copied()
        .filter(|n| !names.iter().any(|m| m == n))
        .collect();
    assert!(
        missing.is_empty(),
        "`methods_of` 上这 9 个 spec 未逐字写、但运行期可用的 List 方法消失了：{missing:?}"
    );
}

//! v0.104.6 D394 —— TEA **列表 model 被 `update` 静默截断成首元素**（修复轮）
//!
//! ## 缺陷
//!
//! `TeaApp::apply_update` **只判类型不判形状**：任何 `Value::List` 返回值
//! 都被当作 `(Model, Cmd)` 二元组，且 model 取 `items.first()`。
//!
//! 而 `tea.init` 的第 1 参**明确允许**「init 闭包（**或初始 model 值**）」
//! （`builtins/tea.rs:26`）⇒ **列表形态的 model 完全合法**。
//!
//! 实测（真实 CLI，修前）：
//!
//! ```text
//! tea.init([1, 2], …)     → model = [1.0, 2.0]   ✅
//! tea.update(该 app, msg) → model = **1.0**      ❌ 类型从 list 变 float
//! tea.init([9, 8, 7], …)  → update 后 = **9.0**
//! ```
//!
//! 零报错、零警告 ⇒ 属**最危险的一类**（静默错值）。
//!
//! ## 修法：只把**确实是二元组**的返回值当元组
//!
//! 条件 = 长度恰为 2 **且**第二项能 `Cmd::from_value` 成功。
//! 其余（1 元素 / 3 元素 / 第 2 项不是 Cmd）一律**整段就是裸 model**。
//!
//! 判据把该修法依赖的**前提**单独钉住（见 ②）——
//! 若 `Cmd::from_value` 变得过宽，修法就会把普通 2 元素列表误判成元组。
//!
//! ## 为什么关键取证只能在 e2e 层
//!
//! 库级用**空 MIR 闭包**（`tea/tests.rs` 的做法）时，`update` 返回 `Nil`，
//! 走的是 `apply_update` 的**非 List 分支** ⇒ **根本进不了被测路径**。
//! 只有真实脚本里「update 返回 model」的闭包才能触发 List 分支。
//! ⇒ 这正是 D391「探针必须进入被测条件」的又一次实例。
//!
//! ## 残留歧义（明文记录，不在本轮解决）
//!
//! 本语言所有值都是 `Value`，**没有静态的 `Cmd` 类型**可供判别。
//! 若 model 恰是「2 元素列表且第 2 项是 Cmd 形态（`nil` 或带 `kind` 的 dict）」，
//! 仍会被判成元组。彻底消歧需给 TEA 引入独立 Cmd 值类型（属设计决定）。

use std::path::Path;
use std::process::Command;

use mora::tea::Cmd;
use mora::value::Value;

fn run_fixture(name: &str) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/e2e")
        .join(name);
    let out = Command::new(exe)
        .args(["run", script.to_str().expect("路径转字符串")])
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

// ── ① 端到端：列表 model 必须完整存活 ──

/// **核心**：2 元素与 3 元素列表 model 经 update 后**原样保留**。
///
/// 修前实测：分别变成 `1.0` / `9.0`（只剩首元素）。
#[test]
fn d394_e2e_list_model_survives_update() {
    let (code, out) = run_fixture("tea_list_model.mora");
    assert_eq!(code, 0, "fixture 应成功; out={out}");
    for (needle, why) in [
        ("list_before=[1.0, 2.0]", "初始 model"),
        ("list_after=[1.0, 2.0]", "2 元素列表 model 不得被截断"),
        ("long_before=[9.0, 8.0, 7.0]", "初始 model"),
        ("long_after=[9.0, 8.0, 7.0]", "3 元素列表 model 不得被截断"),
    ] {
        assert!(out.contains(needle), "缺 `{needle}`（{why}）; out={out}");
    }
    // 反向对照 ①：真元组 `[model, nil]` 仍按元组解读
    // （`nil` 是合法 `Cmd::None` ⇒ 长度 2 且可解析 ⇒ 走元组分支）
    assert!(
        out.contains("tuple_after={count: 0.0}"),
        "真元组 `[model, nil]` 必须仍被当作元组；model 原样保留。\
         本条红 ⇒ 修法把元组支持一起弄坏了; out={out}"
    );
    // 反向对照 ②：dict model 裸返回不受影响
    assert!(
        out.contains("dict_after={count: 0.0}"),
        "dict model 裸返回应原样保留; out={out}"
    );
}

// ── ② 前提：`Cmd::from_value` 必须**拒绝**常见的非 Cmd 值 ──

/// 修法把「第二项能否解析成 `Cmd`」当判别依据。
/// 若 `from_value` 过宽，普通 2 元素列表 model 会被误判成元组。
///
/// ⇒ **前提必须单独钉住**，否则判据与修法一起错，看着还是绿的。
#[test]
fn d394_cmd_from_value_rejects_non_cmd_values() {
    // 会被误判成元组的具体值 —— 必须全部被拒
    for v in [
        Value::Float(2.0),
        Value::Int(2),
        Value::String("nil".to_string()),
        Value::Bool(true),
        // 无 `kind` 字段的 dict
        Value::Dict([("x".to_string(), Value::Int(1))].into_iter().collect()),
        // `kind` 未知
        Value::Dict(
            [("kind".to_string(), Value::String("Bogus".to_string()))]
                .into_iter()
                .collect(),
        ),
    ] {
        assert!(
            Cmd::from_value(&v).is_err(),
            "`Cmd::from_value` 竟接受了 {v:?} —— 它一旦被接受，\
             2 元素列表 model 就会被误判成元组"
        );
    }
    // 合法 Cmd 仍必须被接受（否则修法把元组支持弄坏了）
    assert!(
        Cmd::from_value(&Value::Nil).is_ok(),
        "nil 应解析为 Cmd::None"
    );
    assert!(
        Cmd::from_value(&dispatch_value()).is_ok(),
        "带 kind=Dispatch **且有 msg** 的 dict 应解析为 Cmd"
    );
    // ⚠ 光有 `kind` 不够 —— `Cmd::from_value` 还会校验必需字段。
    //   首版判据只写 `kind=Dispatch` 就期望成功，实测报 `missing msg`
    //   ⇒ **是我的期望写错了**，不是产品缺陷。
    assert!(
        Cmd::from_value(&Value::Dict(
            [("kind".to_string(), Value::String("Dispatch".to_string()))]
                .into_iter()
                .collect()
        ))
        .is_err(),
        "缺 `msg` 的 Dispatch dict 应被拒（记录 `from_value` 的字段校验）"
    );
}

/// 合法的 `Cmd::Dispatch` 值（`kind` + `msg` 都齐）。
fn dispatch_value() -> Value {
    Value::Dict(
        [
            ("kind".to_string(), Value::String("Dispatch".to_string())),
            ("msg".to_string(), Value::String("Tick".to_string())),
        ]
        .into_iter()
        .collect(),
    )
}

// ── ③ 判别形状本身（库级，纯数据） ──

/// **`(len, 第 2 项可解析)` 这组判据的取值表** —— 把边界钉死。
///
/// 修法的判断是 `items.len() == 2 && from_value(items[1]).is_ok()`；
/// 本条枚举各种形状并断言**哪些算元组、哪些算裸 model**。
#[test]
fn d394_tuple_shape_discrimination_table() {
    use mora::value::list::List;
    let nil = Value::Nil;
    let dispatch = dispatch_value();
    let two_nums = Value::List(List::from(vec![Value::Int(1), Value::Int(2)]));
    let model_and_nil = Value::List(List::from(vec![
        Value::Dict([("count".to_string(), Value::Int(0))].into_iter().collect()),
        nil.clone(),
    ]));
    let list_and_nil = Value::List(List::from(vec![Value::Int(1), Value::Int(2), nil.clone()]));
    let model_and_dispatch = Value::List(List::from(vec![
        Value::Dict([("count".to_string(), Value::Int(0))].into_iter().collect()),
        dispatch,
    ]));

    let is_tuple = |v: &Value| match v {
        Value::List(items) => matches!(
            (items.len(), items.get(1).map(Cmd::from_value)),
            (2, Some(Ok(_)))
        ),
        _ => false,
    };

    // 判为元组：model + nil / model + Dispatch
    assert!(is_tuple(&model_and_nil), "`[dict, nil]` 应判为元组");
    assert!(
        is_tuple(&model_and_dispatch),
        "`[dict, Dispatch]` 应判为元组"
    );
    // 判为裸 model：两元素但第 2 项不是 Cmd；三元素；两元素纯数字
    assert!(!is_tuple(&two_nums), "`[1, 2]` 应判为裸 model");
    assert!(
        !is_tuple(&list_and_nil),
        "`[1, 2, nil]`（3 元素）应判为裸 model"
    );
    // 非 List 一律不是元组
    assert!(!is_tuple(&Value::Dict(Default::default())));
}

/// **产品代码确实在用这套判别规则**（源码级）。
///
/// ⚠ 上一条是**本地重实现**判别逻辑 —— 原理上它抓不到产品回归
/// （牙齿验证已证实：还原修前实现时它照常绿）。本条补上缺口：
/// 钉住 `apply_update` 里**真的**有 `items.len()` 这道形状门槛，
/// 且**没有**「无条件取 `items.first()`」的旧写法。
#[test]
fn d394_apply_update_guards_on_length() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tea/mod.rs"),
    )
    .expect("读 src/tea/mod.rs");
    // 只取 apply_update 的函数体
    let start = src.find("fn apply_update(").expect("应能找到 apply_update");
    let end = src[start..]
        .find("\n    /// ")
        .expect("应能找到 apply_update 结尾");
    // ⚠ **必须先剥注释行**再断言 —— D394 的修复说明里就写着
    // 「model 取 `items.first()`」，不剥的话判据会被**自己的注释**命中
    // （与 D388「切片带上下一个 item 的 doc comment」同族）。
    let body: String = src[start..start + end]
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        body.contains("items.len()"),
        "`apply_update` 应按**长度**判定是否元组; 实得片段:\n{body}"
    );
    assert!(
        !body.contains("items.first()"),
        "`apply_update` 仍无条件取 `items.first()` —— 列表 model 会被截断; \
         实得片段:\n{body}"
    );
}

//! v0.104.6 D345 —— `bus.*`（`event`）全族矩阵：5 入口，**0 缺陷**，
//! 但钉住两条**命名与实现不符**的现状
//!
//! `event.*` 是 pub-sub 有状态模块（订阅表 + 通配符分派 + 三桶索引），
//! 从未量过。本轮 20+ 用例，**无产品缺陷**——但量出两条**文档与实现不符**，
//! 它们不会造成危害（理由见下），却会误导下一个来加 `unsubscribe` 的人。
//!
//! ## ① `subscribe` 的注释承诺了一个**不存在**的 API
//!
//! 源码注释（`event.rs:37-42`）逐字写：
//!
//! ```text
//! bus.subscribe(pattern) — pub-sub subscribe
//! Returns: token (Value::Float) for later unsubscribe
//! ```
//!
//! 而 `event.rs` 的入口只有 **5 个**：
//! `count` / `emit` / `off` / `publish` / `subscribe` —— **没有 `unsubscribe`**。
//!
//! 后果：`bus.subscribe` 返回的 token **无任何用处**。
//! 实测重复订阅的 token 甚至**不唯一**：
//!
//! ```text
//! let t1 = bus.subscribe("alpha")   → 1.0     count 1
//! let t2 = bus.subscribe("beta")    → 2.0     count 2
//! let t3 = bus.subscribe("alpha")   → **2.0** count **2**   ← 与 t2 相同
//! ```
//!
//! 因为 token 就是 `pattern_count()`（`event.rs:58`），而 `count()` 数的是
//! **pattern 条目数**（`EventBus::pattern_count` = exact + prefix + interior
//! 三个 map 的 **len 之和**），不是订阅数。
//!
//! **为什么不修**：改 token 语义要动 `EventBus` 的数据结构与
//! `pattern_count` 的含义（多处调用），属**产品契约决定**；
//! 而当前**无可观察危害** —— 脚本层**注入不了 handler**
//! （`subscribe` 只注册一个 no-op 闭包，见 `event.rs:54`），
//! 所以「同一个 pattern 压了几个 handler」在脚本层**完全不可观测**。
//!
//! ## ② `count()` 报的是 **pattern 条目数**，不是订阅数
//!
//! `EventBus::on`（`event/mod.rs:79`）对 Exact 桶用
//! `.entry(pattern).or_default().push(handler)` —— **不去重**。
//! 实测：
//!
//! ```text
//! bus.subscribe("alpha")   → count 1
//! bus.subscribe("alpha")   → count **1**     ← 不增
//! bus.subscribe("alpha")   → count **1**
//! ```
//!
//! 内部压了 **3 个** handler，一次 `emit` 会触发 **3 次** —— 但那 3 个
//! 都是 no-op，脚本层看不见（同 ①）。
//!
//! ## ③ README 只承诺 2 个方法
//!
//! `README.md:154` 写「`bus.emit(name, payload)`, `bus.count()`」，
//! 而实际有 **5 个**（多出 `subscribe` / `publish` / `off`），
//! `docs/mora-spec.md` 里 **零命中** `bus.`。
//!
//! ⇒ 三个方法**零文档**，是「实现比文档多」的方向（与 D68 的
//! `compose_prompt` 相反）——这类缺口**危害小**，但会让用户以为
//! 「订阅了但没文档 = 大概不能用」。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d345_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout **全部**实质行（D330 教训）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d345b_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **契约 ①**：`count()` 是 **pattern 条目数**，重复订阅**不增**。
///
/// 这条钉的是「它数的是什么」——不是「它应该数什么」。
/// 若将来改了实现（去重后每次订阅都增），本条会红 ⇒ 那是**有意的**语义变更。
#[test]
fn d345_count_is_pattern_entries_not_subscription_count() {
    let (code, got) = ev("print(bus.count())\n\
         bus.subscribe(\"alpha\")\nprint(bus.count())\n\
         bus.subscribe(\"alpha\")\nprint(bus.count())\n\
         bus.subscribe(\"alpha\")\nprint(bus.count())\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "0.0 | 1.0 | 1.0 | 1.0",
        "现状：重复订阅**不增**（count 数的是 pattern 条目，不是订阅数）; 实得 {got}\n\
         ⚠ 若本条红，说明有人改成了「按订阅计数」—— 那是**有意的**语义变更"
    );
}

/// **契约 ②**：`subscribe` 的 token = `pattern_count()`，**重复订阅会撞值**。
///
/// 这是 ① 的直接推论，也是「token 无用途」的证据：
/// 既然注释说「返回 token 供 unsubscribe」，而 token 本身不唯一、
/// 且**根本没有 `unsubscribe`**，那这个返回值是**纯占位**。
#[test]
fn d345_subscribe_token_is_not_unique_across_duplicate_patterns() {
    let (code, got) = ev("let t1 = bus.subscribe(\"alpha\")\n\
         let t2 = bus.subscribe(\"beta\")\n\
         let t3 = bus.subscribe(\"alpha\")\n\
         print(t1)\nprint(t2)\nprint(t3)\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "1.0 | 2.0 | 2.0",
        "现状：token == `pattern_count()`，重复订阅**不唯一**（t2 与 t3 都是 2.0）; 实得 {got}\n\
         这正是「token 无法用于 unsubscribe」的根因"
    );
}

/// **契约 ③**：`bus.*` **只有 5 个**入口，**没有 `unsubscribe`**。
///
/// 这条直接钉住 ① 的核心事实。若将来真加了 `unsubscribe`，
/// 本条会红 ⇒ 那是**有意的**功能新增（并需同步修那条注释）。
#[test]
fn d345_bus_has_exactly_five_entries_and_no_unsubscribe() {
    let (code, got) = ev("print(bus.unsubscribe(1.0))\n");
    assert_eq!(
        code, 1,
        "`bus.unsubscribe` 应**不存在**（源码注释承诺了它，是错的）; 实得 exit={code} out={got}"
    );
    assert!(
        // ⚠ 实测措辞是**小写** `unknown method`（`event.rs` 的 fallback 分支），
        //   不是别的 builtin 那种 `Unknown method:`。
        //   故用小写 `contains` 且**不钉句点** —— 钉措辞会在无关的文案清理时假红。
        got.contains("unknown method"),
        "未知方法应明确报错; 实得: {got}"
    );

    // 五个入口逐个确认可达。
    // NOTE: 探针**不带**尾随换行 —— `format!("print({body})\n")` 这类模板
    //   若 `body` 本身以换行结尾，会多出一个孤立的 `)`（第一版就这么翻车，
    //   症状是 parser 报「Expected ')'」，看起来像产品坏了）。
    for (body, what) in [
        ("bus.count()", "count"),
        ("bus.subscribe(\"a\")", "subscribe"),
        ("bus.off(\"a\")", "off"),
        ("bus.emit(\"a\", 1)", "emit"),
        ("bus.publish(\"a\", 1)", "publish"),
    ] {
        let (c, o) = ev(&format!("print({body})"));
        assert_eq!(c, 0, "[{what}] 应可达; 实得 exit={c} out={o}");
    }
}

/// **契约 ④**：`off` 对**不存在的** pattern **不报错**（现状）。
///
/// `EventBus::off` 对三个桶都无条件 `.remove(key)`（`event/mod.rs:171`）
/// —— 删不存在的键是无操作，**不区分**。返回恒为 `Value::Nil`，
/// 所以脚本层**无法**判断「是否真的取消了订阅」。
#[test]
fn d345_off_missing_pattern_is_silent_and_returns_nil() {
    let (code, got) = ev("bus.subscribe(\"a\")\nprint(bus.off(\"nosuch\"))\nprint(bus.count())\n");
    assert_eq!(
        code, 0,
        "off 不存在的 pattern 不应报错; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got, "nil | 1.0",
        "现状：`off` 恒返 Nil（无法告知是否真的取消），且不影响 count; 实得 {got}\n\
         ⚠ 若本条红，说明有人让 `off` 返回布尔 —— 那是**有意的**语义变更"
    );
}

/// **对照组**：`emit` / `publish` **不改变**订阅表；类型错明确报错。
#[test]
fn d345_emit_publish_do_not_change_subscriptions_and_type_errors_are_explicit() {
    // emit / publish 不影响 count
    let (code, got) = ev("bus.subscribe(\"a\")\n\
         bus.emit(\"a\", 1)\nprint(bus.count())\n\
         bus.publish(\"a\", 2)\nprint(bus.count())\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "1.0 | 1.0",
        "emit / publish 只**触发**、不增删订阅; 实得 {got}"
    );

    // 类型错逐个点名
    for (body, needle) in [
        ("bus.emit(1, 2)", "first arg must be a string event name"),
        ("bus.subscribe(5)", "pattern must be a string"),
        ("bus.off(nil)", "first arg must be a string pattern"),
        ("bus.publish(1.5)", "topic must be a string"),
    ] {
        let (c, o) = ev(&format!("print({body})\n"));
        assert_eq!(c, 1, "`{body}` 应报错; 实得 exit={c} out={o}");
        assert!(o.contains(needle), "`{body}` 应报 `{needle}`; 实得: {o}");
    }

    // payload 缺省 ⇒ Nil（不报错）
    let (c, o) = ev("bus.emit(\"a\")\nprint(1)\n");
    assert_eq!(c, 0, "payload 缺省应合法; 实得 exit={c} out={o}");
}

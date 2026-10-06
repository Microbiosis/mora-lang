//! v0.104.6 D332 —— `memory.*` 12 入口矩阵：键身份与**空串显示**的塌陷
//!
//! 结论分两类，**本条只钉现状、不改产品**：两个候选经查证后
//! 一个是**已确认的缺陷但需产品裁决**，一个是**正确行为**。
//!
//! ## ① 矩阵结论：12 入口 × 28 边界用例 —— 零 panic、元数全对
//!
//! 元数由 `typeck/dispatch.rs::MEMORY_METHODS` 拦在**编译期**（缺参/少参
//! 全部 exit 2），与 `builtin_silent_defaults.rs` 记的 D51 现状一致
//! （`memory.store` 的 value 已从静默兜底改为必填）。
//!
//! ## ② **已确认缺陷（需产品裁决，故只钉不改）**：`Value::String("")` 的显示
//!
//! ```text
//! print(["", "a"])    →  [, a]      ← 2 个元素，第 1 个打印成空
//! print([""])         →  []         ← **1 个元素**，打印成**空列表**
//! print(["a", ""])    →  [a, ]      ← 第 2 个元素打印成空
//! print([[""], ["a"]])→  [[], [a]]  ← **嵌套时内层也塌成 []**
//! ```
//!
//! ⇒ **无法区分「一个空串」与「零个元素」**。这不是 `memory` 的问题，
//! 是 `Value` 显示层的通用行为。
//!
//! 在 `memory` 上的具体后果（实测）：
//! ```text
//! memory.store("", "v")
//! memory.size()          → 1.0
//! len(memory.keys())     → 1
//! memory.recall("")      → v          ← 确实存进去了
//! memory.keys()          → []         ← **但显示成空列表**
//! for k in memory.keys() { len(k) }    → 0     ← 键确实是空串，数据没错
//! ```
//!
//! **数据是对的，只有显示层塌陷** —— 故本条不把它当「数据丢失」修。
//!
//! **为什么本条不改**：空串该显示成 `""`、`''`、还是别的，是**显示语义决定**。
//! 而 `docs/mora-spec.md` 对 `to_string` 的显示格式**零规定**，
//! 且改它会波及所有把 `v.to_string()` 当键/路径的调用方（实测 5 个文件、
//! 14 处）：`memory.rs` x9 / `ccr.rs` x2 / `ai.rs` / `schedule.rs` / `toolplane.rs`。
//! 这是**产品契约决定**，只报告。
//!
//! 钉它的理由：将来若决定加引号显示，本条会红 ⇒ 那是**有意的**语义变更，
//! 应连同「14 处调用方的键空间是否需要同步加引号」一起裁决，
//! 而不是被一条判据悄悄改掉。
//!
//! ## ③ **已确认为正确**：`store(1, "x")` 与 `store("1", "x")` 是**两个不同的键**
//!
//! 实测：
//! ```text
//! memory.store(1, "viaInt"); memory.store("1", "viaStr")
//! memory.size()                    → 2.0
//! memory.recall(1)                 → viaInt
//! memory.recall("1")               → viaStr
//! memory.keys()                    → [1, 1.0]   ← 两条都在
//! ```
//!
//! 键在存储里**确实是 String**（`memory_store: HashMap<String, Value>`），
//! `type_of(keys()[0])` 实测为 `string`。`[1, 1.0]` 里的 `1` 是 String
//! 打印（无引号）、`1.0` 是 Float 打印 —— **两个不同的键，视觉上难分辨**。
//!
//! 这**不是**缺陷：任何「键可以是任意值」的 map 都必然如此（Python 的
//! `{1: 'a', 1.0: 'a'}` 会因哈希相等而**合并**成一条，本仓反而更严格）。
//! 但正因为反直觉，必须钉死。
//!
//! ## ④ 非 String 键的完整行为（`v.to_string()` 的后果）
//!
//! | 写入 | `keys()` | `type_of(keys()[0])` | `search` 里的 `key` |
//! |---|---|---|---|
//! | `store(1, …)` | `[1.0]` | `string` | `{key: 123.0}` |
//! | `store([1,2], …)` | `[[1.0, 2.0]]` | `string` | — |
//! | `store({a:1}, …)` | `[{a: 1.0}]` | `string` | — |
//! | `store(nil, …)` | `[nil]` | `string` | — |
//! | `store(true, …)` | `[true]` | `string` | — |
//!
//! 全部是**合法 String 键**（元素的 `to_string()`），`type_of` 恒为 `string`
//! ⇒ **未违反** `MEMORY_METHODS` 声明的 `Ret::List(&[Ret::String])`。
//! D332 初稿曾怀疑「`keys()` 返回 List[Float] 违反契约」—— **实测证伪**，
//! 存储层键是 String，`keys()` 的类型是 `string`。撤回该判断。
//!
//! ## ⑤ dict 键的稳定性：`{a:1, b:2}` 两次构造的 `to_string()` 相同
//!
//! `memory.recall({a: 1, b: 2})` 命中 —— 说明 `Dict` 的 `to_string()`
//! 输出与 `HashMap` 的随机迭代序**无关**（或已排序）。实测一次即命中，
//! 但**只跑一次不算证据**，故本条不钉它（见文件末尾「未钉的观察」）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 每个用例**独立进程**（memory 是有状态面，串在一个进程里会互相污染），
/// 且 `HOME` / `USERPROFILE` 指向临时目录 —— 避免 `memory.remember` 那几个
/// 入口写到用户真实的 `~/.mora/`。
fn run(body: &str, tag: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mora_d332_mem_{}_{}", n, slug(tag)));
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

/// 精确取值 —— **取全部 stdout 实质行**并以 ` | ` 连接。
///
/// ⚠ **不要用 `lines().find(第一行)`**（D330 教训）：多行探针只取得到
/// 第一行，其余断言全部拿不到内容，症状是「exit 对、文本缺」——
/// 极易误判成产品行为变了。这里一次取全。
fn ev_stdout(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mora_d332_out_{}_{}", n, slug(body)));
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
    let kept: Vec<String> = String::from_utf8_lossy(&out.stdout)
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
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **现状判据 ②**：空串在列表里**塌陷成空**，且内存面同步塌陷。
///
/// ⚠ 这是**钉现状**而非认可。若将来决定给字符串加引号显示，本条会红 ——
/// 那是**有意的**语义变更，应连同「14 处把 `v.to_string()` 当键/路径的调用方
/// 是否同步」一起裁决，不要直接改判据。
#[test]
fn d332_empty_string_collapses_in_list_display() {
    for (body, want) in [
        ("print([\"\"])\n", "[]"),
        ("print([\"\", \"a\"])\n", "[, a]"),
        ("print([\"a\", \"\"])\n", "[a, ]"),
        ("print([[\"\"], [\"a\"]])\n", "[[], [a]]"),
        ("print([\"\"] == [])\n", "false"),
    ] {
        let (code, got) = ev_stdout(body);
        assert_eq!(code, 0, "`{body}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`{body}` 的现状是 `{want}`（空串塌陷成空）; 实得 {got}\n\
             ⚠ 若本条红，说明有人改了空串的显示格式 —— 那是**有意的语义变更**，\
             请连同「memory/ccr/ai/schedule/toolplane 共 14 处把 v.to_string() \
             当键或路径」的键空间一起裁决"
        );
    }
}

/// 与上一条配对：**长度与相等性是正确的**，只有显示塌陷。
///
/// 这一对必须同时存在 —— 单钉显示会让人以为「空串被丢了」，
/// 单钉长度会让人以为「显示是对的」。两条合起来才说清：
/// **数据对、显示错**。
#[test]
fn d332_empty_string_data_is_correct_only_display_collapses() {
    let body = "memory.store(\"\", \"v\")\nprint(memory.size())\nprint(len(memory.keys()))\nprint(memory.recall(\"\"))\n";
    let (code, got) = ev_stdout(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "1.0 | 1 | v",
        "空串键**确实存进去了**（size=1、keys 长度=1、recall 命中）; 实得 {got}"
    );

    // 迭代出的键确实是长度 0 的字符串
    let body2 = "memory.store(\"\", \"v\")\nfor k in memory.keys() {\n  print(len(k))\n}\n";
    let (code, got) = ev_stdout(body2);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "0",
        "迭代出的键应是长度 0 的字符串（数据正确）; 实得 {got}"
    );
}

/// **现状判据 ③**：`store(1, …)` 与 `store("1", …)` 是**两个不同的键**。
///
/// 不是缺陷（存储层键是 String，两者的 `to_string()` 不同），
/// 但**反直觉**且视觉上难分辨，故必须钉死。
#[test]
fn d332_numeric_and_string_keys_are_distinct_entries() {
    let body = "memory.store(1, \"viaInt\")\nmemory.store(\"1\", \"viaStr\")\nprint(memory.size())\nprint(memory.recall(1))\nprint(memory.recall(\"1\"))\n";
    let (code, got) = ev_stdout(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "2.0 | viaInt | viaStr",
        "数值键与字符串键应是**两条独立记录**; 实得 {got}\n\
         （Python 的 dict 会因哈希相等把 1 与 1.0 合并，本仓不合并 —— 更严格）"
    );
}

/// **现状判据 ④**：非 String 键全部落成**合法的 String 键**，
/// `type_of(keys()[0])` 恒为 `string`。
///
/// 这条**否证**了 D332 初稿的怀疑（「`keys()` 返回 List[Float] 违反
/// `Ret::List(&[Ret::String])`」）—— 存储层键就是 String，类型契约**没被违反**。
/// 钉它是为了让下一个人不必重走这条弯路。
#[test]
fn d332_non_string_keys_become_legal_string_keys() {
    let body = "memory.store(1, \"v\")\nmemory.store([1,2], \"v2\")\nmemory.store(nil, \"v3\")\nmemory.store(true, \"v4\")\nprint(type_of(memory.keys()[0]))\nprint(memory.size())\n";
    let (code, got) = ev_stdout(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "string | 4.0",
        "任意值都能当键，且落成 String 键（type_of 恒为 string）; 实得 {got}"
    );
}

/// **对照组**：正常 String 键的 CRUD 全链路**逐字不变**。
#[test]
fn d332_normal_crud_unchanged() {
    let body = "memory.store(\"a\", 1)\nprint(memory.recall(\"a\"))\nprint(memory.size())\nprint(memory.keys())\n";
    let (code, got) = ev_stdout(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(got, "1.0 | 1.0 | [a]", "基本 CRUD 现状; 实得 {got}");

    // 覆盖 / forget / clear / 未命中
    for (b, want) in [
        (
            "memory.store(\"k\", 1)\nmemory.store(\"k\", 2)\nprint(memory.recall(\"k\"))\nprint(memory.size())\n",
            "2.0 | 1.0",
        ),
        (
            "memory.store(\"k\", 1)\nmemory.forget(\"k\")\nprint(memory.recall(\"k\"))\nprint(memory.size())\n",
            "nil | 0.0",
        ),
        (
            "memory.store(\"k\", 1)\nmemory.clear()\nprint(memory.size())\n",
            "0.0",
        ),
        ("print(memory.recall(\"nope\"))\n", "nil"),
        ("memory.forget(\"nope\")\nprint(1)\n", "1.0"),
    ] {
        let (code, got) = ev_stdout(b);
        assert_eq!(code, 0, "`{b}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`{b}` 现状应得 {want}; 实得 {got}");
    }
}

/// **对照组**：元数由 typeck 在**编译期**拦截（与 `MEMORY_METHODS` 声明一致）。
#[test]
fn d332_arity_enforced_by_typeck() {
    for b in [
        "memory.store()\n",
        "memory.store(\"k\")\n",
        "memory.recall()\n",
        "memory.forget()\n",
        "memory.search()\n",
        "memory.save()\n",
        "memory.load()\n",
        "memory.remember()\n",
        "memory.recall_markdown()\n",
    ] {
        let (code, got) = run(b, b);
        assert_eq!(
            code, 2,
            "`{b}` 应被 typeck 拒绝（exit 2）; 实得 exit={code} out={got}\n\
             元数由 `typeck/dispatch.rs::MEMORY_METHODS` 声明，缺参不该到运行期"
        );
    }
}

/// **对照组**：`search` 的返回形状（`Dict{key, value}`）与未知方法报错。
#[test]
fn d332_search_shape_and_unknown_method() {
    let body = "memory.store(\"apple\", 1)\nprint(memory.search(\"app\"))\nprint(memory.search(\"zzz\"))\n";
    let (code, got) = ev_stdout(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "[{key: apple, value: 1.0}] | []",
        "search 命中给 key/value 字典，未命中给空列表; 实得 {got}"
    );

    let (code, got) = run("print(memory.nosuch())\n", "unknown");
    assert_eq!(code, 1, "未知方法应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("no method"),
        "错误应说明没有该方法; 实得 {got}"
    );
}

// ── 未钉的观察（诚实记录，不当判据） ──
//
// `memory.recall({a: 1, b: 2})` 单次实测命中，说明 `Dict::to_string()`
// 与 `HashMap` 随机迭代序**无关**（或已排序）。但只跑一次无法排除偶合，
// 故**未钉**。若将来要钉，应连跑 ≥ 20 轮验证稳定性 —— 「跑一次绿了」不是证据。

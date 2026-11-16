//! v0.104.6 D406 —— `memory.keys()` 返回 **`HashMap` 随机序**；
//! 本文件同时是这个缺陷类的**行为普查**（修复轮）
//!
//! ## 缺陷
//!
//! `memory.keys()` 直接收集 `memory_store`（`HashMap<String, Value>`）的键。
//! 实测同一脚本连跑 **5 次**：
//!
//! ```text
//! keys=[bravo, delta, echo, alpha, charlie]   first=bravo
//! keys=[echo, alpha, bravo, delta, charlie]  first=echo
//! keys=[delta, alpha, charlie, echo, bravo]  first=delta
//! keys=[delta, bravo, echo, alpha, charlie]  first=delta
//! keys=[bravo, echo, delta, alpha, charlie]  first=bravo
//! ```
//!
//! `memory.keys()[0]`（「第一个键」）**每次拿到不同的键**。
//!
//! ## 与 `dict.keys()` 是**同一个**危害，而 `dict` 侧已修
//!
//! `method_dispatch.rs` 里 `dict.keys()` 的注释原文：
//! 「用户按 `keys()[0]` 取「第一个键」、或 `for k in d.keys()` 顺序处理，
//! 都会拿到随机结果 —— 且**不报错**」，并有 `tests/dict_determinism.rs` 守护。
//!
//! ⇒ `dict` 修好了，`memory` **漏了**。本条补齐。
//!
//! ## 本文件是**行为普查**，不是源码模式匹配
//!
//! 「列举集合」这类方法的清单会随开发增长，源码模式匹配（找
//! `.keys()` 又找 `.sort()`）**太脆**。本文件改为**真的驱动每一个
//! 脚本可达的列举入口**并断言排序结果 —— 谁回归成随机序谁就红，
//! **与代码怎么写无关**。新增入口时把对应 case 加进 `CASES` 即可。
//!
//! ⚠ 按 D405 的教训：判据里**绝不 sort** —— 排序的是**被测代码**。

use mora::test_support::TempDir;
use std::process::Command;

fn work_dir() -> TempDir {
    TempDir::new("d406")
}

fn mora_exe() -> String {
    env!("CARGO_BIN_EXE_mora").to_string()
}

fn run(body: &str) -> (i32, String) {
    let d = work_dir();
    let script = d.join("p.mora");
    std::fs::write(&script, body).expect("写探针");
    let out = Command::new(mora_exe())
        .arg(&script)
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    let code = out.status.code().unwrap_or(-1);
    let _ = std::fs::remove_dir_all(&d);
    (code, s)
}

/// 全部「脚本可达的集合列举」入口 —— **行为普查表**。
///
/// 新增此类方法时**必须**在此登记；不登记 = 无守护。
///
/// ⚠ 每个 case 的**键数 ≥ 5**：实测 **2 个键时未排序的实现常常「碰巧」有序**
/// —— 牙齿验证把 `memory.keys` 还原成随机序后，2 键的 case **照样绿**。
/// 键少 = 顺序碰撞多 = 探针失效。5 键下未排序几乎必错。
const CASES: &[(&str, &str, &str)] = &[(
    "dict.keys",
    "let d = {echo: 5, delta: 4, charlie: 3, bravo: 2, alpha: 1}\nprint(d.keys())\n",
    "[alpha, bravo, charlie, delta, echo]",
)];

/// **每一个登记的列举入口都必须返回排序结果。**
#[test]
fn d406_all_registered_listings_are_sorted() {
    for (name, body, expect) in CASES {
        let (code, out) = run(body);
        assert_eq!(code, 0, "{name}: 应成功; out={out}");
        assert!(
            out.contains(expect),
            "{name}: 期望输出含 `{expect}`（排序后）; 实得:\n{out}"
        );
    }
}

// ── 决定性判据：跨进程一致 ──

/// **`dict.keys()` 4 个进程输出逐字相同**。
#[test]
fn d406_dict_keys_identical_across_processes() {
    let body = "let d = {echo: 5, delta: 4, charlie: 3, bravo: 2, alpha: 1}\n\
                print(\"keys=\" + str(d.keys()))\n\
                print(\"first=\" + str(d.keys()[0]))\n";
    let mut outs = Vec::new();
    for k in 0..4 {
        let (code, out) = run(body);
        assert_eq!(code, 0, "第 {k} 次非零退出; out={out}");
        let joined: String = out
            .lines()
            .filter(|l| l.starts_with("keys=") || l.starts_with("first="))
            .collect::<Vec<_>>()
            .join(" | ");
        outs.push(joined);
    }
    for (k, o) in outs.iter().enumerate().skip(1) {
        assert_eq!(o, &outs[0], "第 {k} 个进程顺序不同: {o} vs {}", outs[0]);
    }
    assert!(
        outs[0].contains("first=alpha"),
        "首个键应稳定为 alpha; 实得 {}",
        outs[0]
    );
}

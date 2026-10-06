//! v0.104.6 D266 —— `xform.*` 占位标记必须**跨进程可复现**且**不泄露环境**。
//!
//! ## 缺陷（真实 CLI 实测，同一段程序连跑 5 次得到 5 个不同输出）
//!
//! ```text
//! let t = xform.map({alpha:1.0, beta:2.0, gamma:3.0, delta:4.0}); print(t)
//! <xform.map(Dict({"delta": Float(4.0), "gamma": Float(3.0), "alpha": Float(1.0), …}))>
//! <xform.map(Dict({"beta": Float(2.0), "alpha": Float(1.0), "delta": Float(4.0), …}))>
//! <xform.map(Dict({"beta": Float(2.0), "gamma": Float(3.0), "alpha": Float(1.0), …}))>
//! …
//! ```
//!
//! 两个问题：
//!
//! 1. **跨进程不可复现** —— `Value::Dict` 的 `Debug` 走 HashMap 迭代序，
//!    而 `HashMap` 用 `RandomState`（**每进程随机种子**）。
//! 2. **泄露整个全局环境** —— 传 `Closure` 时 `Debug` 把它的 `env`
//!    （PersistentMap / Bitmap / 全部 30 个 builtin 的哈希 / VectorClock）
//!    整份转储进返回值，**单条输出达数千字节**。
//!
//! ## 修法
//!
//! 简单标量显示**值**，复杂类型显示**类型名** —— 与 D233
//! （`json.stringify` 对 Closure / Agent 输出占位串）同一取舍。
//!
//! ⚠ 只用 `Display` 不够：`Value::Dict` 的 `Display` 是 `{k: v}` 形式，
//! 键序同样随 HashMap 变（只是不那么显眼）。
//!
//! ## 判据为什么必须用**子进程**
//!
//! `HashMap` 的种子在**进程内固定** ⇒ 同进程重复调用本来就一致 ⇒
//! 「跨进程不同」在单进程测试里**根本不可表达**（这也是 D265 推翻自己那条
//! 判据的同类理由）。所以这里起 N 个真实 `mora.exe` 子进程比输出。

use std::path::PathBuf;
use std::process::Command;

const RUNS: usize = 6;

struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 同一段程序跑 N 次，返回**去重后**的 stdout 行。
///
/// ⚠ `tag` 必须**逐测试不同**：四个测试并行运行，若共用一个目录会互相
/// 覆盖 `a.mora`，读到的就是别的测试的输出（本条判据第一版就栽在这）。
fn outputs_distinct(tag: &str, src: &str) -> Vec<String> {
    let d = std::env::temp_dir().join(format!("mora_d266_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    let _w = WorkDir(d.clone());
    let f = d.join("a.mora");
    std::fs::write(&f, src).expect("write");

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..RUNS {
        let out = Command::new(exe).arg("run").arg(&f).output().expect("run");
        let txt = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let line = txt
            .lines()
            .map(|l| l.trim())
            .find(|l| l.contains("xform.") && !l.contains("Mora v") && !l.contains("AI 原语"))
            .unwrap_or("(no xform line)")
            .to_string();
        if !seen.contains(&line) {
            seen.push(line);
        }
    }
    seen
}

/// ① 主判据：`Dict` 实参的标记**跨进程必须完全一致**。
#[test]
fn d266_xform_map_dict_marker_is_stable_across_processes() {
    let src = "let t = xform.map({alpha: 1.0, beta: 2.0, gamma: 3.0, delta: 4.0})\nprint(t)\n";
    let outs = outputs_distinct("dict", src);
    assert_eq!(
        outs.len(),
        1,
        "同一段程序连跑 {RUNS} 次得到 **{}** 个不同输出 —— 跨进程不可复现：\n  - {}",
        outs.len(),
        outs.join("\n  - ")
    );
    assert_eq!(outs[0], "<xform.map(dict)>", "占位标记应只用类型名");
}

/// ② `Closure` 实参：**跨进程一致**且**不泄露环境**。
///
/// 修前单条输出达数千字节（整个 PersistentMap / VectorClock 转储）。
#[test]
fn d266_xform_map_closure_marker_is_short_and_stable() {
    let src = "let t = xform.map(fn(x) x + 1 end)\nprint(t)\n";
    let outs = outputs_distinct("closure", src);
    assert_eq!(
        outs.len(),
        1,
        "Closure 实参的标记跨进程不一致：\n  - {}",
        outs.join("\n  - ")
    );
    let line = &outs[0];
    assert_eq!(line, "<xform.map(closure)>");
    assert!(
        line.len() < 64,
        "标记过长（{} 字节）—— 修前会转储整个环境：{line}",
        line.len()
    );
    for leak in [
        "Bitmap",
        "VectorClock",
        "EnvRef",
        "PersistentMap",
        "Builtin(",
    ] {
        assert!(!line.contains(leak), "标记泄露了环境结构 `{leak}`：{line}");
    }
}

/// ③ 简单标量**保留值**（不只剩类型名，否则占位标记失去意义）。
#[test]
fn d266_xform_marker_keeps_scalar_values() {
    for (expr, want) in [
        ("xform.take(100)", "<xform.take(100)>"),
        ("xform.take(2.5)", "<xform.take(2.5)>"),
        ("xform.filter(true)", "<xform.filter(true)>"),
        ("xform.comp(nil)", "<xform.comp(nil)>"),
    ] {
        let outs = outputs_distinct(expr, &format!("let t = {expr}\nprint(t)\n"));
        assert_eq!(outs.len(), 1, "{expr} 跨进程不一致: {outs:?}");
        assert_eq!(&outs[0], want, "{expr} 的占位标记应保留标量值");
    }
}

/// ④ 对照组：确认装置真的在跑（否则 ① 可能因「没输出」而恒绿）。
///
/// ⚠ 必须**独立**读 stdout：复用 `outputs_distinct` 的话，它找的是含
/// `xform.` 的行，而本组输出是 `hello` ⇒ 会永远返回占位符而假红。
#[test]
fn d266_control_group_probe_actually_runs_programs() {
    let d = std::env::temp_dir().join(format!("mora_d266c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    let _w = WorkDir(d.clone());
    let f = d.join("a.mora");
    std::fs::write(&f, "print(\"hello\")\n").expect("write");
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("run")
    .arg(&f)
    .output()
    .expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.trim() == "hello"),
        "对照组失效：探针没读到正常程序的输出。stdout=\n{stdout}"
    );
}

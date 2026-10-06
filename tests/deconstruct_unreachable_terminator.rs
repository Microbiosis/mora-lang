//! v0.104.6 D260 —— `deconstruct` 遇到 `Terminator::Unreachable` 必须**不发射**，
//! 与 `Terminator::Return(None)` 保持一致。
//!
//! ## 缺陷
//!
//! 同一个 `match` 里，**两个都表示「这个块没有显式返回值」**的 terminator
//! 却被降维成了**两种不同的 MIR 指令**：
//!
//! ```text
//! deconstruct.rs:301   Return(None) => Label(usize::MAX)   // skipped —— 不发射
//! deconstruct.rs:307   Unreachable   => Return(None)        // ← 真的发射
//! ```
//!
//! 而 301 行上方的注释**已经把后果写清楚了**：
//!
//! > 发射 `Return(None)` 会在块首就短路（顶层块 Label 后第一条指令即
//! > `Return(None)`），使隐式返回载体永远无法执行 → 返回值变成 Nil。
//!
//! ⇒ 307 行违反了**同一文件自己写下的契约**。
//!
//! ## 触发路径
//!
//! `TailCallOptPass`（`--opt=2` 独有）把尾调用块的 terminator 改成
//! `Unreachable` → 307 行把它变成**真正的** `Return(None)` → 那条指令被
//! 发射到块尾 → DAG 在块首短路 ⇒ **整块都不执行**。
//!
//! 实测（修前，`--opt=2`，22 个程序差分）：
//!
//! | 程序 | opt=1 | 修前 opt=2 | 修后 opt=2 |
//! |---|---|---|---|
//! | `let a = 1` + `print(a)` | `1.0` ✓ | **空** ❌ | `1.0` ✓ |
//! | `d.get("a")` | `1.0` ✓ | **空** ❌ | `1.0` ✓ |
//! | `len(d)` / `len(xs)` / `len(s)` | ✓ | **空** ❌ | ✓ |
//!
//! 破坏面：**opt=2 坏 11/22（50%）→ 6/22（27%）**，且修后 opt=2 与 opt=1 的
//! 破坏集合**完全相同**（都只剩 `for` 循环族，那是 D254–D258 另一条链）。
//!
//! ## 为什么这一族的症状是「**副作用也没了**」
//!
//! 不是「返回值变成 Nil」—— 末值两种情况都是 Nil（`print` 本身返回 Nil）。
//! 而是被发射的 `Return(None)` 让 DAG 在**块首**短路 ⇒ 块里的
//! `Define` / `Var` / `Call` 一条都没跑 ⇒ 连 `print` 的输出都没有。

use mora::mir::ssa::Terminator;

/// ① **主判据（有牙齿）**：`Unreachable` 降维后**不得**产出 `MirInst::Return`。
///
/// 直接打 `deconstruct` 的 terminator 映射，不经 CLI，快且精确。
#[test]
fn d260_unreachable_terminator_is_not_emitted() {
    // `Unreachable` 与 `Return(None)` 语义相同（都「没有显式返回值」），
    // 降维结果必须一致。
    let ssa = {
        let (f, _w) = mora::cli::compile_and_opt("print(\"A\")\n", None).expect("compile");
        let mut s = mora::mir::ssa::construct(&f);
        for b in &mut s.blocks {
            if matches!(b.terminator, Terminator::Return(_)) {
                b.terminator = Terminator::Unreachable;
            }
        }
        s
    };
    let out = mora::mir::ssa::deconstruct(&ssa);

    let emitted_returns: Vec<_> = out
        .body
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, mora::mir::MirInst::Return(_)))
        .map(|(i, _)| i)
        .collect();
    assert!(
        emitted_returns.is_empty(),
        "`Terminator::Unreachable` 被降维成了**真正的** `MirInst::Return`，\
         指令位置 {emitted_returns:?}。它会在块首短路，使整块（Define/Var/Call）\
         都不执行 —— 这正是 `--opt=2` 下 `let a = 1; print(a)` 连输出都没有的原因。\
         它必须和 `Return(None)` 一样被跳过（deconstruct.rs 的 301 行已有该契约）。\n\
         body:\n  {}",
        out.body
            .iter()
            .enumerate()
            .map(|(i, x)| format!("{i:>3}: {x:?}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// ② **端到端判据**：`--opt=2` 下这 5 个基础构造必须与 `--opt=off` 一致。
///
/// 它们在修前**全部**输出为空（D259 实测），是这一族的完整覆盖面。
#[test]
fn d260_opt2_preserves_basic_define_var_programs() {
    const CASES: &[(&str, &str)] = &[
        ("let-bind", "let a = 1\nprint(a)\n"),
        ("dict-get", "let d = {a: 1, b: 2}\nprint(d.get(\"a\"))\n"),
        ("dict-len", "let d = {a: 1, b: 2}\nprint(len(d))\n"),
        ("list-len", "let xs = [1,2,3]\nprint(len(xs))\n"),
        ("string-len", "let s = \"abc\"\nprint(len(s))\n"),
    ];
    const NOISE: &[&str] = &[
        "Mora v",
        "AI ",
        "Built-in",
        "v0.15",
        "⚠",
        "AI 原语",
        "显式 API",
        "Trait",
    ];

    // `Drop` 守卫：assert 失败（panic）时末尾的清理**执行不到**，
    // 而牙齿验证正是靠让它失败 ⇒ 残留会稳定复现。
    struct WorkDir(std::path::PathBuf);
    impl Drop for WorkDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = std::env::temp_dir().join(format!("mora_d260_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let _work = WorkDir(dir.clone());

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let run = |src: &str, opt: Option<&str>| -> (i32, Vec<String>) {
        let f = dir.join("a.mora");
        std::fs::write(&f, src).expect("write");
        let mut c = std::process::Command::new(exe);
        if let Some(o) = opt {
            c.arg(o);
        }
        let out = c.arg("run").arg(&f).output().expect("run");
        let lines = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.contains(n)))
            .collect();
        (out.status.code().unwrap_or(-1), lines)
    };

    for (name, src) in CASES {
        let (c0, o0) = run(src, None);
        let (_, o2) = run(src, Some("--opt=2"));
        assert_eq!(c0, 0, "{name}: opt=off 应成功");
        assert!(!o0.is_empty(), "{name}: 对照组失效 —— opt=off 本就没有输出");
        assert_eq!(
            o2, o0,
            "{name}: `--opt=2` 的输出必须与 opt=off 一致（D260 修前这里是**空**）"
        );
    }
}

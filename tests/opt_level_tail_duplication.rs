//! v0.104.6 D303：`--opt=1/2` 下 `let` 之后的**尾部被重复执行**（阶梯式）。
//!
//! ## 现象（实测）
//!
//! | 程序 | 默认（`None`） | `--opt=1/2` |
//! |---|---|---|
//! | `print("A")` `print("B")`（无 let） | `A, B` ✅ | `A, B` ✅ |
//! | `let a = 5i` + **1** 条语句 | `A` ✅ | `A` ✅ |
//! | `let a = 5i` + **2** 条语句 | `A, B` ✅ | `A, B, B` ❌ |
//! | `let a = 5i` + **3** 条语句 | `A, B, C` ✅ | `A, B, C, B, C, C` ❌ |
//! | `let a = 5i` + **4** 条语句 | `A, B, C, D` ✅ | `A, B, C, D, B, C, D, C, D, D` ❌ |
//!
//! 呈**阶梯式**：第 k 条语句被执行 k 次。`let` 是否被使用**无关**
//! （`let a = 5i` 未被读也复现），普通赋值 `a = 5i` 同样复现。
//!
//! ## 与 D261 的关系（已核对，不重复计数）
//!
//! D261 记的 opt 档位不一致是「6/22 个**含 for 循环**的程序」，
//! 预置判据 `tests/opt_repair_acceptance.rs` 已覆盖（它整体 `#[ignore]`）。
//! 本条把那 22 个程序原样跑了一遍（从判据文件逐字提取，避免手抄走样）：
//! 提取到的 16 个里三档一致 13 个，破坏的 3 个全是 for 循环
//! （`for-print-item` / `for-print-const` / `for-empty`，opt≥1 下**输出全空**）。
//! **阶梯式重复一个都没出现** ⇒ 本条是 D261 之外的**新签名**。
//!
//! ## 为什么默认档是安全的
//!
//! `src/mir/ssa.rs:117-122` 写明：优化 pass「**未证明对所有程序安全前，默认关闭**
//! 作可回退逃生舱」，而 `OptLevel::default()` 就是 `None`。
//! ⇒ 本条是那个「已知不安全」清单上的**又一条数据**，不是默认路径的缺陷。
//!
//! ## 本文件是现状判据
//!
//! 根因落在 SSA 改名后的 DAG 执行器（已量到：两档 MIR 结构同形，
//! 但 `None` 的 dst 单调递增、`Basic` 的非单调），属**优化器核心手术**，
//! 本轮只钉现状不修 —— 与 D276 同理，不在没有充分验证时动优化器核心。

use std::process::Command;

fn run(src: &str, tag: &str, opt: bool) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("mora_d303_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut c = Command::new(exe);
    if opt {
        c.arg("--opt=1");
    }
    let out = c.arg(&p).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                && l.len() < 24
                && !l.starts_with("Mora v")
                && !l.contains("AI:")
                && !l.contains("AI 原语")
                && !l.contains("显式 API")
                && !l.contains("Trait 系统")
                && !l.contains("Built-in")
                && !l.contains("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
        })
        .collect()
}

/// **现状判据**：`--opt=1` 下 `let` 之后的尾部被阶梯式重复执行。
///
/// ★ v0.104.6 D308 已修好：本判据已翻转为断言**正确行为**（三档 opt 均应与默认档一致）。
#[test]
fn opt_level_does_not_duplicate_the_tail_after_a_let() {
    // v0.104.6 D308：本条原先是**现状判据**（钉住 `--opt=1` 的阶梯式重复），
    // 现已按其原注释的指示翻转为断言**正确行为**。
    //
    // 根因：`dag_analyze` 的可达性遍历跑在「边全部建完」**之前。
    // SSA（`--opt=1` 及以上）会在函数体 pc 0 插入一个 `Label(0)`，
    // 而该 Label 节点的出边到后面「Label 透明化」那一步才被 push，
    // 遍历到它时**一条出边都没有** ⇒ `reachable = {0}` ⇒
    // 执行器 `seq_preds` 的 `reachable` 过滤把**所有**链式 Sequence 边丢掉，
    // 只剩 `dag_analyze` Step 3 的 Effect 扇出在排序 ⇒ 第 k 条语句的节点跑 k 遍。
    let cases: &[(&str, &[&str])] = &[
        ("let+1句", &["A"]),
        ("let+2句", &["A", "B"]),
        ("let+3句", &["A", "B", "C"]),
        ("let+4句", &["A", "B", "C", "D"]),
    ];
    for (tag, want) in cases {
        let src = format!(
            "let a = 5i\n{}",
            (0..want.len())
                .map(|i| format!("print(\"{}\")\n", (b'A' + i as u8) as char))
                .collect::<String>()
        );
        for (opt, label) in [(false, "默认档"), (true, "--opt=1")] {
            let got = run(&src, tag, opt);
            assert_eq!(
                got, *want,
                "[{tag}] `{label}` 下的尾部被重复执行（D308 修的正是这个）：{got:?}"
            );
        }
    }
}

/// 对照组：**默认档**必须始终正确 —— 它才是「优化 pass 未证明安全前的逃生舱」
/// （`ssa.rs:117-122`），任何退化都会打到所有默认用户。
#[test]
fn default_opt_level_is_correct_for_all_these_shapes() {
    for (tag, src, want) in [
        ("let+1", "let a = 5i\nprint(\"A\")\n", vec!["A"]),
        (
            "let+2",
            "let a = 5i\nprint(\"A\")\nprint(\"B\")\n",
            vec!["A", "B"],
        ),
        (
            "let+3",
            "let a = 5i\nprint(\"A\")\nprint(\"B\")\nprint(\"C\")\n",
            vec!["A", "B", "C"],
        ),
        (
            "let 未使用",
            "let a = 5i\nprint(\"A\")\nprint(\"B\")\n",
            vec!["A", "B"],
        ),
        (
            "let 被使用",
            "let a = 5i\nprint(a)\nprint(\"B\")\n",
            vec!["5", "B"],
        ),
        ("无 let", "print(\"A\")\nprint(\"B\")\n", vec!["A", "B"]),
        (
            "赋值非 let",
            "let a = 0i\na = 5i\nprint(\"A\")\nprint(\"B\")\n",
            vec!["A", "B"],
        ),
    ] {
        let got = run(src, "def", false);
        assert_eq!(got, want, "[{tag}] 默认档必须正确; 实得 {got:?}");
    }
}

/// D261 已知：含 `for` 循环的程序在 `--opt=1/2` 下**输出全空**。
///
/// 该条已由 `tests/opt_repair_acceptance.rs`（整体 `#[ignore]`）覆盖，
/// 本文件只把「默认档仍正确」这一半钉住 —— 免得将来有人误以为
/// for 循环在默认档也会空输出。
#[test]
fn for_loop_is_fine_at_the_default_level() {
    let got = run("for i in [1, 2, 3]\n  print(i)\nend\n", "for", false);
    assert_eq!(
        got,
        vec!["1.0", "2.0", "3.0"],
        "默认档的 for 循环必须正确; 实得 {got:?}"
    );
}

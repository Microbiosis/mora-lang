//! v0.104.6 D474：块形态 / 声明形态 / 真实 `.mora` 的**行为契约**。
//!
//! ## 与 D315 版的差别
//!
//! D315 版用「两条编译路径（9 层 vs `emit.rs`）输出逐行相同」作判据。
//! **D474 把差分机制整个退役** —— parser 不再发射 MirInst（只产 witness），
//! MIR 全部由 9 层产出 ⇒「参考实现」这一侧**已不存在**，那条 A/B 会变成
//! **自己跟自己比**（恒真）。
//!
//! ⇒ 判据换成**钉死的期望输出**：每种形态在每个优化档位下都必须产出
//! **具体且正确**的值。这比「两条路径一致」**更强** —— 后者只能发现
//! 「两条路径不一致」（两条一起错到同一个错值时反而绿）。
//!
//! ## 三个优化档位（`--opt` 关 / 1 / 2）
//!
//! 保留这一维的理由：它判的是**「优化器只能改效率，不能改语义」** ——
//! 一条与编译路径**无关**的、不变量。
//!
//! ## 覆盖的两类形态
//!
//! - **块形态**（span / observe / parallel / prompt / document）：D92 原文
//!   警告「若差分被放宽/移除，这些块里的语句会**不执行**」。D315 用 30 个
//!   A/B 组合否证了它；本文件把该验证固化为**期望输出**断言。
//! - **声明形态**（msg / struct / enum）：D315 从「回落」翻成「通过」的那一组。

use mora::test_support::TempDir;
use std::process::Command;

/// 一条形态的期望结果。
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 运行成功（退出码 0），stdout **逐行**等于给定值。
    Out(&'static str),
    /// 编译期类型错误（退出码 2）。
    TypeErr,
}

/// 跑一个程序，返回 `(stdout, stderr, 退出码)`。
fn run(tag: &str, src: &str, opt: Option<&str>) -> (String, String, i32) {
    let dir = TempDir::new(tag);
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let mut c = Command::new(env!("CARGO_BIN_EXE_mora"));
    if let Some(l) = opt {
        c.arg(format!("--opt={l}"));
    }
    let out = c.arg("run").arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

/// 块形态 —— 保留在语言中的块形态在每个优化档位下的期望输出。
const BLOCK_FORMS: &[(&str, &str, Expect)] = &[
    (
        "parallel-3stmt",
        "parallel\n  print(1)\n  print(2)\n  print(3)\nend\nprint(\"after\")\n",
        Expect::Out("1\n2\n3\nafter"),
    ),
    (
        // `parallel` 块的值是 **Nil** ⇒ 绑到 `let` 上是类型错误。
        "parallel-in-value",
        "let x = parallel\n  print(1)\nend\nprint(\"after\")\n",
        Expect::TypeErr,
    ),
];

/// 声明形态 —— 每种形态在每个优化档位下的期望输出。
const DECLARATION_FORMS: &[(&str, &str, Expect)] = &[
    (
        "struct-1print",
        "struct P\n  x: number\nend\nprint(1)\n",
        Expect::Out("1"),
    ),
    (
        "struct-3print",
        "struct P\n  x: number\nend\nprint(1)\nprint(2)\nprint(3)\n",
        Expect::Out("1\n2\n3"),
    ),
    (
        "enum-1print",
        "enum E\n  A\n  B\nend\nprint(1)\n",
        Expect::Out("1"),
    ),
    (
        "enum-3print",
        "enum E\n  A\n  B\nend\nprint(1)\nprint(2)\nprint(3)\n",
        Expect::Out("1\n2\n3"),
    ),
    (
        "struct-then-let",
        "struct P\n  x: number\nend\nlet y = 1\nprint(y)\nprint(y + 1)\n",
        Expect::Out("1\n2"),
    ),
    (
        "enum-then-for",
        "enum E\n  A\n  B\nend\nlet t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
        Expect::Out("6"),
    ),
];

/// 块形态与声明形态：**每个优化档位下**都必须产出钉死的期望输出。
///
/// 这同时钉住两件事：
/// 1. 形态本身的行为正确（块里的语句**确实执行了**、值**确实对**）；
/// 2. 优化器不改语义（三个档位输出**逐行相同**）。
#[test]
fn block_and_declaration_forms_produce_expected_output_on_every_opt_level() {
    let mut failures: Vec<String> = Vec::new();
    for (group, forms) in [("block", BLOCK_FORMS), ("declaration", DECLARATION_FORMS)] {
        for (name, src, expect) in forms {
            for opt in [None, Some("1"), Some("2")] {
                let lbl = opt.unwrap_or("off");
                let (stdout, stderr, code) = run(&format!("{group}_{name}_{lbl}"), src, opt);
                let got = stdout.trim_end_matches('\n');
                let ok = match expect {
                    Expect::Out(want) => code == 0 && got == *want,
                    Expect::TypeErr => code == 2,
                };
                if !ok {
                    failures.push(format!(
                        "  [{group}/{name} opt={lbl}] 期望 {expect:?}\n    实得: exit={code} stdout={got:?}\n    stderr: {}",
                        stderr.trim_end()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "块形态 / 声明形态的行为与期望不符（{} 条）：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 仓库里**每一个** e2e fixture 都必须跑得通 —— **除了 7 个故意的负例**。
///
/// D315 版这里断言的是「不再回落」（stderr 里不出现 `[9layer]`）。D474 起
/// **没有回落机制**了 ⇒ 那条判据恒真 ⇒ 换成**真正可观察**的判据：
/// fixture 必须**成功运行**。
///
/// 7 个负例是**故意**失败的（名字里就写着 bad / missing / partial / win
/// —— 它们测的是「错误路径的诊断」），故单独列出并**钉死退出码**：
/// 退出码变了 ⇒ 那条诊断路径变了，需要人来确认。
const KNOWN_FAILURES: &[(&str, i32)] = &[
    // 类型错误（编译期）
    ("lisp.mora", 2),
];

#[test]
fn every_e2e_fixture_runs_successfully() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("读 e2e fixture 目录")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mora"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "e2e fixture 目录空了？本测试会**静默变绿**"
    );

    let mut failures: Vec<String> = Vec::new();
    for f in &files {
        let out = Command::new(env!("CARGO_BIN_EXE_mora"))
            .arg("run")
            .arg(f)
            .output()
            .expect("跑 mora");
        let code = out.status.code().unwrap_or(-1);
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match KNOWN_FAILURES.iter().find(|(n, _)| *n == name) {
            Some((_, want)) => {
                if code != *want {
                    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
                    failures.push(format!(
                        "  {name}  负例的退出码应恰为 {want}，实得 {code}\n    {}",
                        stderr.trim_end()
                    ));
                }
            }
            None => {
                if code != 0 {
                    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
                    failures.push(format!("  {name}  exit={code}\n    {}", stderr.trim_end()));
                }
            }
        }
    }
    // 负例清单里的每个名字都必须真的存在于 fixture 目录里（防清单与目录脱节）。
    for (name, _) in KNOWN_FAILURES {
        assert!(
            files.iter().any(|f| {
                f.file_name()
                    .map(|n| n.to_string_lossy() == *name)
                    .unwrap_or(false)
            }),
            "KNOWN_FAILURES 里的 `{name}` 不在 e2e fixture 目录里 —— \
             fixture 改名或删除了？请同步更新本清单（否则这条断言会**静默失效**）。"
        );
    }
    assert!(
        failures.is_empty(),
        "e2e fixture 跑不通（{} 个）：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

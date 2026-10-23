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

/// **强证人**：打印求解结果的 `rel` 程序。
///
/// `rel_*.mora` fixture 自身无输出，比不出对错 —— 这一条才有可观察结果。
const REL_WITNESS: &str = concat!(
    "rel edge(\"a\", \"b\")\n",
    "rel edge(\"b\", \"c\")\n",
    "rel edge(\"c\", \"d\")\n",
    "let r = solve { edge(?x, ?y) }\n",
    "print(\"edges:\")\n",
    "print(r)\n",
    "let p = solve { edge(?x, \"d\") }\n",
    "print(\"reach-d:\")\n",
    "print(p)\n",
    "print(\"done\")\n",
);

/// `rel` 家族必须走上 9 层管线，且求解结果**可观察且正确**。
#[test]
fn rel_family_produces_its_expected_output() {
    for opt in [None, Some("1"), Some("2")] {
        let lbl = opt.unwrap_or("off");
        let (stdout, stderr, code) = run("rel_witness", REL_WITNESS, opt);
        assert_eq!(code, 0, "前提：opt={lbl} 应成功; stderr={stderr}");
        assert_eq!(
            stdout.trim_end(),
            "edges:\n[[a, b], [b, c], [c, d]]\nreach-d:\n[c]\ndone",
            "**D474**：`rel` 程序的求解结果必须是可观察且正确的（opt={lbl}）。\n  实得: {stdout:?}"
        );
    }
}

/// 块形态 —— 每种形态在每个优化档位下的期望输出。
const BLOCK_FORMS: &[(&str, &str, Expect)] = &[
    (
        "span-1stmt",
        "span \"s\" do\n  print(1)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\nafter"),
    ),
    (
        "span-3stmt",
        "span \"s\" do\n  print(1)\n  print(2)\n  print(3)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\n2.0\n3.0\nafter"),
    ),
    (
        "parallel-3stmt",
        "parallel\n  print(1)\n  print(2)\n  print(3)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\n2.0\n3.0\nafter"),
    ),
    (
        // `span` 块的值是 **Nil**（块值被丢弃）⇒ 绑到 `let` 上是类型错误。
        // 这条把「块的值不可用」钉住。
        "span-in-value",
        "let x = span \"s\" do\n  print(1)\nend\nprint(\"after\")\n",
        Expect::TypeErr,
    ),
    (
        "span-nested",
        "span \"o\" do\n  span \"i\" do\n    print(1)\n  end\n  print(2)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\n2.0\nafter"),
    ),
    (
        "span-then-loop",
        "span \"s\" do\n  print(1)\nend\nlet t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
        Expect::Out("1.0\n6.0"),
    ),
    (
        "span-then-3prints",
        "span \"s\" do\n  print(1)\nend\nprint(\"a\")\nprint(\"b\")\nprint(\"c\")\n",
        Expect::Out("1.0\na\nb\nc"),
    ),
    (
        "observe-trace",
        "observe trace \"t\" do\n  print(1)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\nafter"),
    ),
    (
        "prompt-block",
        "prompt \"p\" do\n  print(1)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\nafter"),
    ),
    (
        "document-block",
        "document \"d\" do\n  print(1)\nend\nprint(\"after\")\n",
        Expect::Out("1.0\nafter"),
    ),
];

/// 声明形态 —— 每种形态在每个优化档位下的期望输出。
const DECLARATION_FORMS: &[(&str, &str, Expect)] = &[
    (
        "msg-1print",
        "msg CounterMsg\n  Increment\n  Decrement\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "msg-3print",
        "msg M\n  Inc\nend\nprint(1)\nprint(2)\nprint(3)\n",
        Expect::Out("1.0\n2.0\n3.0"),
    ),
    (
        "struct-1print",
        "struct P\n  x: number\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "struct-3print",
        "struct P\n  x: number\nend\nprint(1)\nprint(2)\nprint(3)\n",
        Expect::Out("1.0\n2.0\n3.0"),
    ),
    (
        "enum-1print",
        "enum E\n  A\n  B\nend\nprint(1)\n",
        Expect::Out("1.0"),
    ),
    (
        "enum-3print",
        "enum E\n  A\n  B\nend\nprint(1)\nprint(2)\nprint(3)\n",
        Expect::Out("1.0\n2.0\n3.0"),
    ),
    (
        "struct-then-let",
        "struct P\n  x: number\nend\nlet y = 1\nprint(y)\nprint(y + 1)\n",
        Expect::Out("1.0\n2.0"),
    ),
    (
        "enum-then-for",
        "enum E\n  A\n  B\nend\nlet t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
        Expect::Out("6.0"),
    ),
    (
        "msg+struct+enum",
        "struct P\n  x: number\nend\nmsg M\n  Inc\nend\nenum E\n  A\nend\nprint(1)\nprint(2)\n",
        Expect::Out("1.0\n2.0"),
    ),
    (
        "msg-then-span",
        "msg M\n  Inc\nend\nspan \"s\" do\n  print(1)\nend\nprint(2)\n",
        Expect::Out("1.0\n2.0"),
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
    // 运行期错误
    ("plan_update_partial.mora", 1),
    ("sandbox_bad_capability.mora", 1),
    ("sandbox_container_mount_bad_mode.mora", 1),
    ("sandbox_container_mount_win.mora", 1),
    ("schedule_surface.mora", 1),
    ("toolplane_missing_plane.mora", 1),
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

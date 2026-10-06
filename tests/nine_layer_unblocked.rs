//! v0.104.6 D315：9 层管线的**静默回落 11/56 → 1/56** —— 重新应用 D276
//!
//! ## 背景：一条**以某个具体修复为前提**的护栏
//!
//! D276 把 `nested_diffs` 改成按「剔除死 no-op 后」对齐，**随即撤销**。
//! 撤销理由是原文记录的：
//!
//! > 对齐后 `rel_*.mora` 不再回落 ⇒ 改走 9 层路径，而**那条路径是坏的**：
//! > `Runtime error (MIR): … references register 4 but the function only has
//! > 1 register(s)`
//!
//! > 也就是说：**这套「错位」目前是 `rel_*` 唯一的护栏**。
//! > **所以在 9 层 rel 路径修好之前，必须保留错位。**
//!
//! **D314 修好了那个前提**（同一个 `n_regs` 少算 bug），却没有回头看这条
//! 注释。D315 顺着 D314 结尾的「待验」项查下去才发现它早已失效。
//!
//! ## 差异究竟是什么
//!
//! 逐条转储后确认：`pipeline_mir=23` vs `original_mir=26`，差的是 **3 条死
//! no-op** —— emit 路径在每条 `RelDef` **语句**后补一条 `Const(r, Nil)`
//! （语句必须有值），而 r = 0/1/2 **从未被任何指令读**。剔除后两侧
//! **23 vs 23 逐条按类别一一对齐**。这正是 D57 立 `significant_indices`
//! 过滤时描述的那一种，**不是新的放宽**。
//!
//! ## 证据（缺一不可）
//!
//! | 验证 | 规模 | 结果 |
//! |---|---|---|
//! | 56 个真实 `.mora` × 3 档，pipeline vs `MORA_9LAYER=0`(emit) | 168 组合 | **0 差异** |
//! | 10 种**块形态** × 3 档 | 30 组合 | **0 差异** |
//! | `rel` 强证人（打印求解结果） | 1 | 输出**逐行相同** |
//!
//! ⚠ 第一项**单独不够**：`rel_*.mora` fixture 自身**一个 `print` 都没有**，
//! 可观察行为只有「exit 0、无输出」，比不出对错；56 个 fixture 里也**没有**
//! 一个用块形态。第二项与第三项才是 D276 当年缺的那块证据。

use std::process::Command;

const EXE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");

/// 跑一个程序。`force_emit = true` 时用 `MORA_9LAYER=0` 关掉 9 层管线、
/// 强制走 `emit.rs` 路径 —— 用来做**两条编译路径的 A/B**。
fn run(src: &str, tag: &str, opt: Option<&str>, force_emit: bool) -> (i32, Vec<String>, bool) {
    let dir = std::env::temp_dir().join(format!("mora_d315_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let mut c = Command::new(EXE);
    if let Some(l) = opt {
        c.arg(format!("--opt={l}"));
    }
    if force_emit {
        c.env("MORA_9LAYER", "0");
    }
    let out = c.arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let fell_back = text.contains("[9layer]");
    // ⚠ 错误消息里会嵌入**临时目录路径**（如 `…/mora_d315_xxx/p.mora: Failed to
    // parse`），而 A/B 两次用的 tag 不同 ⇒ 路径字符串天然不同。必须先归一化，
    // 否则「两边同样解析失败」也会被判成不一致。头两版没做归一化，踩了这个坑。
    let norm = text.replace(&dir.to_string_lossy().to_string(), "<TMP>");
    let lines = norm
        .lines()
        .map(|l| l.trim().to_string())
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
        .collect();
    (out.status.code().unwrap_or(-1), lines, fell_back)
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

/// **主判据**：`rel` 家族真走上 9 层管线，且两条路径输出**逐行相同**。
#[test]
fn rel_family_runs_on_the_nine_layer_pipeline_with_identical_output() {
    let (dc, got, fell_back) = run(REL_WITNESS, "witness", None, false);
    assert_eq!(dc, 0, "前提：默认档应成功; 实得 {dc}");
    assert_eq!(
        got,
        vec![
            "edges:".to_string(),
            "[[a, b], [b, c], [c, d]]".to_string(),
            "reach-d:".to_string(),
            // 实测值：`solve { edge(?x, "d") }` 命中一条边，print 出该行本身。
            // （此处曾按记忆写成 `[[c]]`，实跑打回 —— 期望值必须来自实测。）
            "[c]".to_string(),
            "done".to_string(),
        ],
        "**D315**：`rel` 程序的求解结果必须是可观察且正确的。\n  实得: {got:?}"
    );
    assert!(
        !fell_back,
        "**D315**：`rel` 家族应当**走 9 层管线**（静默回落应已从 11/56 降到 1/56）。\n\
         若又回落，说明 `nested_diffs` 的死 no-op 对齐被撤销了 —— 参考 D276：\n\
         那次撤销的原因是 9 层 rel 路径会越界崩溃，而该崩溃已由 D314 修复。\n\
         详见 `src/mir/pipeline.rs::nested_diffs` 的 D315 注释。"
    );

    for opt in [None, Some("1"), Some("2")] {
        let lbl = opt.unwrap_or("off");
        let (pc, pipe, _) = run(REL_WITNESS, &format!("pipe_{lbl}"), opt, false);
        let (ec, emit, _) = run(REL_WITNESS, &format!("emit_{lbl}"), opt, true);
        assert_eq!(
            (pc, pipe.clone()),
            (ec, emit.clone()),
            "**D315**：opt={lbl} 下 9 层管线与 emit.rs 两条路径的输出必须**逐行相同**。\n\
             这 10 个程序的生产路径本轮从 emit 换成了管线，这是本次变更最大的\n\
             回归面。\n  管线: {pipe:?}\n  emit : {emit:?}"
        );
    }
}

/// D314 之前处于回落名单里的 10 个程序，现在都必须**不再回落**。
///
/// 仍回落的只有 `tea_standalone.mora`。
#[test]
fn previously_falling_back_programs_now_use_the_pipeline() {
    for fixture in [
        "rel_basic",
        "rel_cons",
        "rel_empty",
        "rel_project",
        "rel_run_limit",
        "rel_single_var",
        "rel_zero_var",
        "prompt_section",
        "export_visibility",
        "import_handle_index_main",
    ] {
        let path = format!(
            "{}/tests/fixtures/e2e/{}.mora",
            env!("CARGO_MANIFEST_DIR"),
            fixture
        );
        let out = Command::new(EXE).arg(&path).output().expect("跑");
        let text = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !text.contains("[9layer]"),
            "**D315**：`{fixture}.mora` 应当走 9 层管线。\n\
             它此前因 `nested_diffs` 的下标错位而静默回落（死 no-op 对齐 +\n\
             D314 修好 `n_regs` 之后应当放行）。stderr 里仍出现 `[9layer]`：\n{}",
            text.lines().find(|l| l.contains("[9layer]")).unwrap_or("")
        );
    }
}

/// **声明形态护栏**：`msg` / `struct` / `enum` 本轮也**从回落翻成了通过**。
///
/// 与块形态同源：emit 在声明型指令后补的 `Const(r, Nil)` 是死 no-op。
/// 实测 10 种形状 × 3 档 = **30 组合**，两条路径输出逐行相同。
///
/// 普查里翻转的 8 条**全部同向**（回落 → 通过），无一条反向退化 —— 若哪天
/// 出现反向条目，`nine_layer_fallback_census.rs` 的
/// `d92b_census_matches_measured_fallback_set` 会先红。
#[test]
fn declaration_forms_behave_identically_on_both_paths() {
    const FORMS: &[(&str, &str)] = &[
        (
            "msg-1print",
            "msg CounterMsg\n  Increment\n  Decrement\nend\nprint(1)\n",
        ),
        (
            "msg-3print",
            "msg M\n  Inc\nend\nprint(1)\nprint(2)\nprint(3)\n",
        ),
        ("struct-1print", "struct P\n  x: number\nend\nprint(1)\n"),
        (
            "struct-3print",
            "struct P\n  x: number\nend\nprint(1)\nprint(2)\nprint(3)\n",
        ),
        ("enum-1print", "enum E\n  A\n  B\nend\nprint(1)\n"),
        (
            "enum-3print",
            "enum E\n  A\n  B\nend\nprint(1)\nprint(2)\nprint(3)\n",
        ),
        (
            "struct-then-let",
            "struct P\n  x: number\nend\nlet y = 1\nprint(y)\nprint(y + 1)\n",
        ),
        (
            "enum-then-for",
            "enum E\n  A\n  B\nend\nlet t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
        ),
        (
            "msg+struct+enum",
            "struct P\n  x: number\nend\nmsg M\n  Inc\nend\nenum E\n  A\nend\nprint(1)\nprint(2)\n",
        ),
        (
            "msg-then-span",
            "msg M\n  Inc\nend\nspan \"s\" do\n  print(1)\nend\nprint(2)\n",
        ),
    ];

    for (name, src) in FORMS {
        for opt in [None, Some("1"), Some("2")] {
            let lbl = opt.unwrap_or("off");
            let (pc, pipe, _) = run(src, &format!("dc_pipe_{name}_{lbl}"), opt, false);
            let (ec, emit, _) = run(src, &format!("dc_emit_{name}_{lbl}"), opt, true);
            assert_eq!(
                (pc, pipe.clone()),
                (ec, emit.clone()),
                "**D315**：声明形态 `{name}` 在 opt={lbl} 下，两条编译路径必须等价。\n\
                 这 3 条与 5 种块形态是 D315 里从「回落」翻成「通过」的全部 8 条。\n\
                 管线: {pipe:?}\n  emit : {emit:?}"
            );
        }
    }
}

/// **块形态护栏**（D92 的后续）：这 5 种块形态本轮**也**走上了管线。
///
/// D92 原文警告「若差分被放宽/移除，这些块里的语句会**不执行**」。
/// D315 实测 10 种形状 × 3 个优化档位 = **30 个组合**，输出**逐行相同**
/// ⇒ 那条缺失的尾部 `Return` 不产生可观察差异（D315 未单独定位其补偿点）。
///
/// 本条把该验证固化为**常驻回归**，而不是像 D276/D92 那样靠一条判据的
/// 绿/红来间接保护 —— 间接保护正是这两次反复的根源。
#[test]
fn block_forms_behave_identically_on_both_paths() {
    const FORMS: &[(&str, &str)] = &[
        (
            "span-1stmt",
            "span \"s\" do\n  print(1)\nend\nprint(\"after\")\n",
        ),
        (
            "span-3stmt",
            "span \"s\" do\n  print(1)\n  print(2)\n  print(3)\nend\nprint(\"after\")\n",
        ),
        (
            "parallel-3stmt",
            "parallel\n  print(1)\n  print(2)\n  print(3)\nend\nprint(\"after\")\n",
        ),
        (
            "span-in-value",
            "let x = span \"s\" do\n  print(1)\nend\nprint(\"after\")\n",
        ),
        (
            "span-nested",
            "span \"o\" do\n  span \"i\" do\n    print(1)\n  end\n  print(2)\nend\nprint(\"after\")\n",
        ),
        (
            "span-then-loop",
            "span \"s\" do\n  print(1)\nend\nlet t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
        ),
        (
            "span-then-3prints",
            "span \"s\" do\n  print(1)\nend\nprint(\"a\")\nprint(\"b\")\nprint(\"c\")\n",
        ),
        (
            "observe-trace",
            "observe trace \"t\" do\n  print(1)\nend\nprint(\"after\")\n",
        ),
        (
            "prompt-block",
            "prompt \"p\" do\n  print(1)\nend\nprint(\"after\")\n",
        ),
        (
            "document-block",
            "document \"d\" do\n  print(1)\nend\nprint(\"after\")\n",
        ),
    ];

    for (name, src) in FORMS {
        for opt in [None, Some("1"), Some("2")] {
            let lbl = opt.unwrap_or("off");
            let (pc, pipe, _) = run(src, &format!("bf_pipe_{name}_{lbl}"), opt, false);
            let (ec, emit, _) = run(src, &format!("bf_emit_{name}_{lbl}"), opt, true);
            assert_eq!(
                (pc, pipe.clone()),
                (ec, emit.clone()),
                "**D315**：块形态 `{name}` 在 opt={lbl} 下，两条编译路径必须等价。\n\
                 D92 曾断言「差分被放宽 ⇒ 块里的语句不执行」；D315 实测 30 组合\
                 全部相同，故把该验证固化为常驻护栏。\n  管线: {pipe:?}\n  emit : {emit:?}"
            );
        }
    }
}

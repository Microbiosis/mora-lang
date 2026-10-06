//! v0.104.6 D398 —— `mora.refine` 产出的 `.refined.<n>.mora`
//! **语法非法、根本无法运行**（修复轮）
//!
//! ## 缺陷
//!
//! `refine` 把指令头写成 `# --- INSTRUCTION (refine iter N): <text>`，
//! 而 **Mora 的行注释记号是 `--`，`#` 不是合法记号**。
//!
//! 实测（真实 CLI，直接跑 `refine` 产物那种形态的文件）：
//!
//! ```text
//! # --- INSTRUCTION (refine iter 1): add X
//! print("ok")
//! ```
//!
//! ```text
//! p2.mora: Unexpected character '#' at line 1, column 1
//! exit=2
//! ```
//!
//! ⇒ 整个 refine 工具的产物**跑不起来** —— 而它的用途正是
//! 「产生副本供用户 review / 继续编辑」，产物必须是合法 Mora。
//!
//! ## 关键：这不是取舍，是**笔误**，且测试把 bug 形态钉住了
//!
//! ① 本模块 doc **第 12 行自己写的就是**「副本包含 `-- INSTRUCTION: <text>`
//!    注释行」⇒ **代码与自己的 doc 矛盾**，doc 表达的正是 `--`。
//! ② 既有单测断言 `content.contains("# --- INSTRUCTION ...")`
//!    ⇒ **测试把 bug 固化了**，于是「产物能否运行」这件事**从未被验证过**。
//!
//! ## 判据的核心：**用真实 `mora` 二进制跑一遍产物**
//!
//! 只断言「文件存在 / 含某字符串」是**恒真**的（写什么都能通过）——
//! 必须**执行**它，断言 exit 0 + 输出正确。修前本条会红在
//! `Unexpected character '#'`。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use mora::refine::RefineSession;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 临时工作目录（Drop 时清理）。
struct Work(PathBuf);

impl Work {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mora_d398_{n}_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        Work(d)
    }
    fn write_script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, body).expect("写脚本");
        p
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

fn run_mora(script: &Path) -> (i32, String) {
    let out = Command::new(mora_exe())
        .arg(script)
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

const SCRIPT: &str = "-- 原脚本注释\nprint(\"hello\")\n";

// ── ① 核心：产物必须**真的能跑** ──

/// **`refine` 的产物能被真实 `mora` 执行**（exit 0 + 正确输出）。
///
/// 修前实测：exit 2，`Unexpected character '#' at line 1, column 1`。
#[test]
fn d398_refined_copy_is_runnable_mora() {
    let w = Work::new("runnable");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let step = s.refine("add greeting").expect("refine 应成功");

    let (code, out) = run_mora(&step.refined_path);
    assert_eq!(
        code,
        0,
        "refine 产物**无法运行**（exit {code}）—— 指令头用了非法记号。\n\
         产物首行: {:?}\n完整输出:\n{out}",
        first_line(&step.refined_path)
    );
    assert!(
        out.contains("hello"),
        "产物应执行原脚本逻辑并输出 hello; out={out}"
    );
}

/// **指令头用 Mora 的行注释记号 `--`**（不是 `#`）。
#[test]
fn d398_instruction_header_uses_mora_comment_marker() {
    let w = Work::new("marker");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let step = s.refine("add greeting").expect("refine 应成功");
    let first = first_line(&step.refined_path);
    assert!(
        first.starts_with("--"),
        "指令头首行应以 `--`（Mora 行注释）开头; 实得 {first:?}"
    );
    assert!(
        !first.starts_with('#'),
        "指令头仍用 `#` —— Mora 无此注释记号，产物语法非法; 实得 {first:?}"
    );
    assert!(
        first.contains("INSTRUCTION") && first.contains("add greeting"),
        "指令头应含 INSTRUCTION 与指令文本; 实得 {first:?}"
    );
}

/// **多候选**（`refine_many(3)`）的每个产物**都**能运行、头都用 `--`。
#[test]
fn d398_all_candidates_are_runnable() {
    let w = Work::new("candidates");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let steps = s
        .refine_many("add greeting", 3)
        .expect("refine_many 应成功");
    assert_eq!(steps.len(), 3, "应产出 3 个候选");
    for (i, st) in steps.iter().enumerate() {
        let (code, out) = run_mora(&st.refined_path);
        assert_eq!(
            code,
            0,
            "候选 {} 产物无法运行（exit {code}）; 首行 {:?}\nout={out}",
            i,
            first_line(&st.refined_path)
        );
        let first = first_line(&st.refined_path);
        assert!(
            first.starts_with("--") && first.contains("candidate "),
            "候选 {} 的指令头应含 `--` 与 candidate 标记; 实得 {first:?}",
            i
        );
    }
    // 三个候选文件名互不相同
    let names: Vec<String> = steps
        .iter()
        .map(|s| {
            s.refined_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mut uniq = names.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "候选文件名应互不相同; 实得 {names:?}");
}

// ── ② 原内容必须逐字保留 ──

/// **指令头之后的正文是原脚本的完整内容**。
///
/// ⚠ 契约由**实测**得出（不是读代码猜的）：内容恒为
/// `指令头 + "\n" + 原内容 + "\n"` —— `refine` 用的是
/// `format!("{}\n{}\n", ...)`，**尾部还有一个 `\n`**。
/// 我第一版把它读成了 `"{}\n{}"` ⇒ 判据写错，实测才纠正过来。
#[test]
fn d398_original_content_preserved_verbatim() {
    let w = Work::new("preserve");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let step = s.refine("x").expect("refine 应成功");
    let content = std::fs::read_to_string(&step.refined_path).expect("读产物");
    let header = first_line(&step.refined_path);
    assert_eq!(
        content,
        format!("{}\n{}\n", header, SCRIPT),
        "产物应逐字等于「指令头 + 原脚本内容」（含尾部换行）"
    );
    // 字节数：原内容 + 指令头行 + 两个换行
    assert!(
        step.refined_bytes > step.original_bytes,
        "产物应比原文大（多出指令头）; refined={} original={}",
        step.refined_bytes,
        step.original_bytes
    );
}

// ── ③ 现状钉：两条**结构性恒定**的字段（只报告不修） ──

/// **`diff_lines_added` 恒 2、`diff_lines_removed` 恒 0**（实测契约）。
///
/// 产物恒为 `指令头 + "\n" + 原内容 + "\n"`，而
/// `format!("{}\n{}\n", …)` 比原文**多 2 行**（指令头 1 行 + 尾部空行 1 行）
/// ⇒ `added = 2`、`removed = 0` 恒成立，与原内容无关。
///
/// 属「结构性恒定的装饰字段」族（同 D395 的 `delete_after_run`）：
/// `removed` **永远**是 0。**无可观察危害**（简化 diff 就是「+2 行」这件事
/// 本身是设计），故只钉现状、不修。
#[test]
fn d398_diff_counts_are_structurally_constant() {
    let w = Work::new("diffzero");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let step = s.refine("x").expect("refine 应成功");
    assert_eq!(
        step.diff_lines_removed, 0,
        "`diff_lines_removed` 变成非 0 —— diff 逻辑已改，结论需重写"
    );
    assert_eq!(
        step.diff_lines_added, 2,
        "产物比原文多 2 行（指令头 + 尾部空行）⇒ added 恒为 2; 实得 {}",
        step.diff_lines_added
    );
}

/// **迭代编号按 `steps.len()+1` 递增** ⇒ 多候选会**跳号**。
///
/// `refine_many(3)` 一次产生 3 个 step（iteration 都记 1），
/// 下一次 `n = steps.len() + 1 = 4` ⇒ 2 与 3 号迭代**不存在**。
///
/// 现状钉：这是实现现状（文件名与 `to_dict` 的 `iteration` 字段都受影响），
/// **不是**本轮要改的（改它属产品语义决定）。
#[test]
fn d398_iteration_number_skips_after_multi_candidate() {
    let w = Work::new("iter");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    let a = s.refine_many("first", 3).expect("第一轮");
    assert!(
        a.iter().all(|st| st.iteration == 1),
        "同一次迭代的候选 iteration 应相同; 实得 {:?}",
        a.iter().map(|x| x.iteration).collect::<Vec<_>>()
    );
    let b = s.refine_many("second", 1).expect("第二轮");
    assert_eq!(
        b[0].iteration, 4,
        "下一轮 iteration 实得 {}（= steps.len()+1，跳过了 2/3）—— \
         若改成按**迭代**计数则应为 2，改动前请先更新本条",
        b[0].iteration
    );
    // 迭代号确实出现在**文件名**里（用户可见）
    let name = b[0]
        .refined_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.contains("refined.4."), "文件名应含迭代号; 实得 {name}");
}

// ── ④ 错误路径 ──

/// **`count` 校验**与**不存在的脚本**都给出点名对象的错误。
#[test]
fn d398_error_paths_name_the_offender() {
    let w = Work::new("errs");
    let script = w.write_script("demo.mora", SCRIPT);
    let mut s = RefineSession::new(&script);
    for bad in [0usize, 27] {
        let e = s
            .refine_many("x", bad)
            .expect_err(&format!("count={bad} 应被拒"));
        assert!(
            e.contains("count must be 1..=26") && e.contains(&bad.to_string()),
            "错误应点名 count 与实际值 {bad}; 实得 {e}"
        );
    }
    // 不存在的脚本
    let mut s2 = RefineSession::new(&w.0.join("nope.mora"));
    let e = s2.refine("x").expect_err("不存在的脚本应报错");
    assert!(
        e.contains("nope.mora") || e.contains("read "),
        "错误应点名脚本路径; 实得 {e}"
    );
}

fn first_line(p: &Path) -> String {
    std::fs::read_to_string(p)
        .expect("读产物")
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

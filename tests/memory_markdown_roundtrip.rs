//! v0.104.6 D220：`memory.remember` 写进 markdown 的内容**读不回来** ——
//! 多行丢行、前缀 `- ` 被吃掉、文本里的 `## x` 能伪造出一个 section（已修）。
//!
//! ## 缺陷
//!
//! `interpreter/builtins/memory.rs` 的 markdown 记忆（`remember` /
//! `recall_markdown`）往返**有损**，且全程 **exit 0、零诊断**：
//!
//! ① **多行文本丢行**：`remember("multi", "line1\nline2\nline3")` 落盘成
//!    三行，但 `recall_markdown` 只收集以 `- ` 开头的行 ⇒ 收回 `line1`，
//!    **line2 / line3 无声消失**。
//! ② **前缀 `- ` 被吃掉**：`recall` 用 `trim_start_matches("- ")`，
//!    它剥掉**所有**前导 `- `。`remember("d", "- leading dash")` 存的是
//!    `- - leading dash`，读回却变成 `leading dash`。
//! ③ **文本能伪造 section**：文本里含 `## fakesection` 时会被
//!    `recall_markdown` / `list_markdown` 当成**真的** section 标题。
//!
//! ## D221：section 存在性用**整文件子串匹配** ⇒ 条目落进**错误的段**
//!
//! 修前 `new_content.contains(&section_header)` 是子串匹配，且追加位置是
//! **文件末尾**而非该段末尾。三个后果（全部实测）：
//!
//! ```text
//! ① 前缀冲突：remember("notes-archive", …) 之后 contains("## notes") 为**真**
//!    ⇒ remember("notes", …) 的条目被追加到 notes-archive 段里，
//!      而 `## notes` 段**从未创建**：
//!        recall_markdown("notes")          = 空
//!        recall_markdown("notes-archive") = 两条都在
//! ② 段不在文件末尾时：追加到文件末尾会落进**下一个**段
//!    （remember("a") / remember("b") / remember("a") 三条即触发）
//! ③ 短名先建时恰好正确 —— 所以这个缺陷**顺序依赖**，极易漏测
//! ```
//!
//! ## 修法
//!
//! ① 续行统一缩进两格（`indent_continuation`）：既不像新 bullet、
//!    也不像新 `## ` 段 ⇒ ③（伪造）也一并解决；
//! ② `find_section` **按整行**精确匹配 + `section_end` 定位**该段末尾**；
//! ③ `recall` 用 `strip_prefix("- ")`（只剥一个）并把缩进续行拼回同一条目。
//!
//! ## 影响面
//!
//! `memory.*` 是 agent 的**跨会话持久记忆**（写 `~/.mora/memory/YYYY-MM-DD.md`），
//! 此前**零测试覆盖**（`tests/` 下无任何 memory/persist 测试）。
//! 记忆写进去却读不回来，且没有任何提示 —— 对一个「记住/回忆」功能是致命的。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d220_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(format!("{name}.mora"));
        std::fs::write(&p, body).expect("写脚本");
        p
    }
    /// 跑一份脚本，返回「程序自己的输出」（剥掉横幅）。
    fn run(&self, name: &str, body: &str) -> String {
        let p = self.script(name, body);
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .current_dir(&self.0)
        .arg(&p)
        // 隔离到本测试的临时目录，**不**碰用户真实的 ~/.mora/memory
        .env("MORA_MEMORY_DIR", &self.0)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{name}] 应正常执行:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("Mora v")
                    && !t.starts_with("AI:")
                    && !t.starts_with("AI 原语")
                    && !t.starts_with("显式 API")
                    && !t.starts_with("Trait 系统")
                    && !t.starts_with("Built-in")
                    && !t.starts_with("v0.15 CLI")
                    && !t.starts_with('⚠')
            })
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" | ")
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **主判据（有牙齿）**：写进 markdown 的内容必须**原样**读回来。
///
/// 修前：`line2` / `line3` 静默丢失；`- leading dash` 变成 `leading dash`。
#[test]
fn d220_multiline_and_dash_prefixed_text_round_trip() {
    let dir = WorkDir::new("roundtrip");
    let out = dir.run(
        "multi",
        "memory.remember(\"multi\", \"line1\\nline2\\nline3\")\n\
         print(memory.recall_markdown(\"multi\"))\n",
    );
    assert_eq!(
        out, "line1 | line2 | line3",
        "[D220] 多行文本**丢行** —— `recall_markdown` 只收集以 `- ` 开头的行，\
         其余各行无声消失（修前只收回 line1）"
    );

    let out2 = dir.run(
        "dash",
        "memory.remember(\"dash\", \"- leading dash\")\n\
         print(memory.recall_markdown(\"dash\"))\n",
    );
    assert_eq!(
        out2, "- leading dash",
        "[D220] 前缀 `- ` 被**吃掉** —— 修前用 `trim_start_matches(\"- \")`，\
         它剥掉**所有**前导 `- `，于是 `- - leading dash` 读回成 `leading dash`"
    );
}

/// **D220 ③**：文本里的 `## x` **不得**变成一个真的 section。
///
/// 修前：能被 `recall_markdown` / `list_markdown` 当成真标题。
#[test]
fn d220_text_cannot_forge_a_section_header() {
    let dir = WorkDir::new("inject");
    let out = dir.run(
        "inject",
        "memory.remember(\"inj\", \"line1\\n## fakesection\\nline3\")\n\
         print(memory.recall_markdown(\"inj\"))\n\
         print(memory.recall_markdown(\"fakesection\"))\n",
    );
    assert_eq!(
        out, "line1 | ## fakesection | line3",
        "文本里的 `## fakesection` 应作为**内容**原样读回；\
         且 `recall_markdown(\"fakesection\")` 必须是空的（第二个 print 无输出）"
    );
    assert!(
        !out.split(" | ").any(|s| s == "fakesection"),
        "文本里的 `## fakesection` **伪造出了一个 section** —— \
         `recall_markdown(\"fakesection\")` 返回了内容。修前续行未缩进，会被当成真标题。\n实际: {out}"
    );
}

/// **D221 主判据**：类别名前缀冲突时，条目必须进**自己的**段。
///
/// 修前：`contains("## notes")` 命中 `## notes-archive` 的子串 ⇒
/// `## notes` 段从未创建，`recall(notes)` 为空而 `recall(notes-archive)` 有两条。
#[test]
fn d221_prefix_category_lands_in_its_own_section() {
    let dir = WorkDir::new("prefix");
    let out = dir.run(
        "prefix",
        "memory.remember(\"notes-archive\", \"in-archive\")\n\
         memory.remember(\"notes\", \"in-notes\")\n\
         print(memory.recall_markdown(\"notes\"))\n\
         print(memory.recall_markdown(\"notes-archive\"))\n",
    );
    assert_eq!(
        out, "in-notes | in-archive",
        "[D221] 前缀冲突 —— 修前 `contains(\"## notes\")` 命中 `## notes-archive` 的\
         **子串**，条目被追加到错误段、且 `## notes` 段从未创建\
         （recall(notes) 为空、recall(notes-archive) 两条都在）"
    );
}

/// **D221 ②**：段不在文件末尾时，追加必须落在**该段末尾**而非文件末尾。
///
/// 修前：追加到文件末尾 ⇒ 落进**下一个**段。
#[test]
fn d221_append_lands_in_that_section_not_at_eof() {
    let dir = WorkDir::new("midsec");
    let out = dir.run(
        "midsec",
        "memory.remember(\"a\", \"a1\")\n\
         memory.remember(\"b\", \"b1\")\n\
         memory.remember(\"a\", \"a2\")\n\
         print(memory.recall_markdown(\"a\"))\n\
         print(memory.recall_markdown(\"b\"))\n",
    );
    assert_eq!(
        out, "a1 | a2 | b1",
        "[D221] 追加落到了**错误**的段 —— 修前一律追加到文件末尾，\
         于是 `a` 的第二条落进了 `b` 段（recall(b) 会多出 a2、recall(a) 只有 a1）"
    );
}

//! v0.104.6 D375 —— `src/document/` 的**六个格式后端**端到端（否定轮，无产品变更）
//!
//! `src/document/backend/` 共 6 个模块 1768 行，但只有 3 个判据
//! （`document_html_blocks` / `document_markdown_blocks` /
//! `document_methods_introspection`）—— **docx / pdf / pptx / png
//! 四个二进制格式无任何判据**。
//!
//! ## 六个后端全部分派到位
//!
//! `document::parse_document`（`mod.rs:47`）按扩展名分派：
//! `pdf` / `md`\|`markdown` / `html`\|`htm` / `pptx` / `docx` / `png`。
//!
//! ⚠ `mod.rs:46` 的注释写着「Tasks 5–7 会分别实现
//! **PdfBackend / MarkdownBackend / HtmlBackend**」—— 只列 3 个，
//! 而实际有 6 个。属**过时注释**，本判据不依赖它。
//!
//! ## 三个二进制 fixture 全部解析成功
//!
//! | 格式 | 页数 | 内容 |
//! |---|---|---|
//! | `sample.pdf` | 1 | bbox 595×842（A4）|
//! | `sample.docx` | 1 | 3 个段落，文字正确 |
//! | `sample.pptx` | 2 | 「Sample Slide 1 / Second slide text」|
//!
//! ## docx 的 `width`/`height` = 0 是**明文设计**，不是缺陷
//!
//! `backend/docx.rs:143-146` 的文档原文：
//!
//! > with width/height = 0 (**placeholder geometry**)…
//! > There is **no** per-text-run bbox in the undoc DOCX output,
//! > so spans use a zero bbox and `score = nil`
//!
//! 本条把该约定**钉住**，防止将来被误判成「尺寸丢失」而「修复」。
//!
//! ## png 的错误消息质量：**带可执行修复指引**
//!
//! ```text
//! document.parse: ocrs engine init error: ocr.load (detection):
//!   ocr.load: model file '…\mora\ocr\text-detection.rten' not found.
//!   Run 'mora-install-ocr' to download, or set MORA_OCR_MODELS_DIR
//! ```
//!
//! 指名了**缺哪个文件** + **两条修复路径**（命令 / 环境变量）——
//! 这是全仓错误消息里质量较高的一类，单独钉住。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d375_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d375_{n}_{}", slug(body)));
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
    let path_str = p.to_string_lossy().into_owned();
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
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

fn fixture(name: &str) -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + name
}

/// 三个二进制 fixture 的**内容**必须被正确解析出来。
///
/// 这条比「exit 0」强得多 —— 只验成功会漏掉「解析出空内容」。
#[test]
fn d375_binary_backends_extract_real_content() {
    for (fmt, expect) in [
        ("docx", "Sample DOCX paragraph 1"),
        ("pptx", "Sample Slide 1"),
    ] {
        let path = fixture(&format!("sample.{fmt}"));
        let (code, got) = ev(&format!(
            "let d = document.parse(\"{path}\")\nprint(d.pages())\n"
        ));
        assert_eq!(code, 0, "`{fmt}` 应解析成功; 实得 exit={code} out={got}");
        assert!(
            got.contains(expect),
            "`{fmt}` 应抽出真实文本 `{expect}`; 实得: {got}"
        );
    }
}

/// **pdf 的页面尺寸**是真实值（A4 = 595×842），不是 0。
///
/// 这是与 docx 的**反向对照** —— docx 用 placeholder 0，pdf 用真实值。
#[test]
fn d375_pdf_reports_real_page_geometry() {
    let path = fixture("sample.pdf");
    let (code, got) = ev(&format!(
        "let d = document.parse(\"{path}\")\nprint(d.pages())\n"
    ));
    assert_eq!(code, 0, "pdf 应解析成功; 实得 exit={code} out={got}");
    assert!(
        got.contains("595") && got.contains("842"),
        "pdf 应报告 A4 尺寸（595×842）; 实得: {got}"
    );
}

/// **docx 的 `width`/`height` = 0 是明文设计**（`docx.rs:143-146`）。
///
/// 本条把它**钉住**：将来若有人看到 0 以为是「尺寸丢失」而擅自"修复"，
/// 这条会提醒先读那段文档。
#[test]
fn d375_docx_uses_placeholder_geometry_by_design() {
    let path = fixture("sample.docx");
    let (code, got) = ev(&format!(
        "let d = document.parse(\"{path}\")\nprint(d.pages())\n"
    ));
    assert_eq!(code, 0, "docx 应解析成功; 实得 exit={code} out={got}");
    assert!(
        got.contains("height: 0") || got.contains("height: 0.0"),
        "docx 的 height 应为 0（`docx.rs:143` 明写 placeholder geometry）; 实得: {got}"
    );
}

/// **不支持的扩展名**必须列出**全部**支持格式。
#[test]
fn d375_unsupported_extension_lists_all_supported_formats() {
    for (name, path) in [
        ("jpg", "/tmp/x.jpg"),
        ("无扩展名", "/tmp/sample"),
        ("txt", "/tmp/x.txt"),
    ] {
        let (code, got) = ev(&format!("let d = document.parse(\"{path}\")\nprint(1)\n"));
        assert_eq!(code, 1, "`{name}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains("unsupported extension"),
            "`{name}` 的诊断应说明扩展名不支持; 实得: {got}"
        );
        // 错误消息必须**列出全部 6 类支持格式**
        for ext in ["pdf", "md", "html", "pptx", "docx", "png"] {
            assert!(got.contains(ext), "诊断应列出支持的 `{ext}`; 实得: {got}");
        }
    }
}

/// **文件不存在**必须报**路径**错误（不是格式错误）。
///
/// ⚠ 只对**受支持**的扩展名成立：`/nope/none.txt` 在**分派层**就被拒
/// （`unsupported extension`），压根不会去读文件 —— 这是**正确**行为
/// （不浪费 IO）。首版把 `.txt` 也放进这条 ⇒ 假红。
#[test]
fn d375_missing_file_reports_path_error() {
    for (name, path) in [
        ("pdf", "/nope/none.pdf"),
        ("docx", "/nope/none.docx"),
        ("pptx", "/nope/none.pptx"),
        ("md", "/nope/none.md"),
    ] {
        let (code, got) = ev(&format!("let d = document.parse(\"{path}\")\nprint(1)\n"));
        assert_eq!(
            code, 1,
            "`{name}` 缺失文件应报错; 实得 exit={code} out={got}"
        );
        assert!(
            !got.contains("unsupported extension"),
            "`{name}`：扩展名受支持，文件不存在**不该**报「扩展名不支持」; 实得: {got}"
        );
        assert!(
            got.contains("cannot read"),
            "`{name}`：应报「无法读取」并指明路径; 实得: {got}"
        );
    }
}

/// **png 需要外部 OCR 模型**，缺模型时的错误消息必须
/// ① 点名缺失的模型文件 ② 给出**可执行的修复路径**。
///
/// 这是全仓错误消息里质量较高的一类，单独钉住。
#[test]
fn d375_png_ocr_error_names_the_fix() {
    // 若本机已装模型，本条自动跳过（判据不该在「环境已就绪」时假红）
    let probe = ev(&format!(
        "let d = document.parse(\"{}\")\nprint(1)\n",
        fixture("sample.png")
    ));
    if probe.0 == 0 {
        // 模型已就绪 ⇒ 解析成功，本条无事可钉
        return;
    }
    assert_eq!(
        probe.0, 1,
        "缺 OCR 模型时应报错; 实得 exit={} out={}",
        probe.0, probe.1
    );
    let got = &probe.1;
    assert!(
        got.contains("ocr") || got.contains("rten"),
        "诊断应点名 OCR 模型; 实得: {got}"
    );
    // **可执行的修复指引**：命令 或 环境变量，至少给一条
    assert!(
        got.contains("mora-install-ocr") || got.contains("MORA_OCR_MODELS_DIR"),
        "诊断必须给出**可执行的修复路径**（命令或环境变量）; 实得: {got}"
    );
}

/// **image 后端只支持 png** —— `jpg` 报的是「格式不支持」，
/// 而**不是**被分派到别的后端。
///
/// `backend/image.rs:156` 显式拒绝非 png；`parse_document` 也只匹配
/// `"png"`。两者**一致**，不是分叉。
#[test]
fn d375_only_png_is_routed_to_the_image_backend() {
    // `jpg` 走的是**扩展名分派**那条路（unsupported extension），
    // 而不是 image 后端的「格式不支持」——因为分派里根本没有 jpg 分支。
    let (code, got) = ev("let d = document.parse(\"/tmp/x.jpg\")\nprint(1)\n");
    assert_eq!(code, 1, "`.jpg` 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("unsupported extension"),
        "`.jpg` 在**分派层**就被拒（不是 image 后端）; 实得: {got}"
    );
}

//! v0.104.6 D402 —— `src/heartbeat/` 首次外部覆盖：解析逻辑**零缺陷**（否定轮）
//! + 模块 doc 的 builtin 名**不存在**（文档更正）+ 一处给「将来接线」的拆雷
//!
//! ## ① 解析逻辑：否定轮
//!
//! `HeartbeatItem::parse` + `parse_heartbeat` 逐条核对**全部正确**：
//! 四种 checkbox 形态（`[ ]` / `[x]` / `[X]` / `[]`）、前导缩进、
//! 非清单行跳过、**行号 1 基**、空清单「vacuously complete」
//! （与 `plan::completion_ratio` 空时返回 1.0 的**兄弟语义一致**）。
//!
//! ## ② 文档更正：doc 声称的 builtin **根本不存在**
//!
//! doc 原本写 `builtin heartbeat.check(path?)`。实测：
//!
//! ```text
//! heartbeat.check("HEARTBEAT.md") → Type error: Unbound variable 'heartbeat'
//! ```
//!
//! ⇒ 没有 `heartbeat` 这个 builtin 模块。实际名字是 `ai.heartbeat(path?)`
//! （`builtins/ai.rs::call_ai_method`），而它**源码不可达**（D59
//! `tests/ai_namespace_reachability.rs`；其单测**直接调 `call_ai_method`**，
//! 绕过了名字解析）。
//! 与 D280 在 `orchestrate_dag` 上修过的是**同一类**文档漂移。
//!
//! ## ③ 给「将来接线」的拆雷：绕过沙箱的文件读
//!
//! `load_heartbeat` / `ai.heartbeat` 接收**调用方给的任意路径**且
//! **不走 `sandbox.check_path`** —— 而所有 `file.*` 带路径入口都走了
//! （D334 / D389 逐一补齐）。
//! ⇒ 一旦接线就等于开了一个**绕过沙箱的文件读**口子。已写进模块 doc。

use std::path::{Path, PathBuf};

use mora::heartbeat::{load_heartbeat, parse_heartbeat};

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {rel} 失败: {e}"))
}

// ── ① 解析矩阵 ──

/// **四种 checkbox 形态**全部被识别，且 done 判定正确。
#[test]
fn d402_checkbox_forms_parsed() {
    let r = parse_heartbeat("- [ ] a\n- [x] b\n- [X] c\n- [] d\n", None);
    assert_eq!(r.total, 4);
    assert_eq!(r.done, 2, "[x] / [X] 应算 done（`[ ]` 与 `[]` 是 pending）");
    assert_eq!(r.pending, 2);
    assert!(!r.is_complete());
    assert_eq!(r.items[0].text, "a");
    assert_eq!(r.items[1].text, "b");
    assert_eq!(r.items[3].text, "d");
}

/// **前导缩进**被容忍（checklist 常缩进在标题下）。
#[test]
fn d402_indented_items_are_parsed() {
    let r = parse_heartbeat("  - [x] deep\n\t- [ ] deeper\n", None);
    assert_eq!(r.total, 2, "缩进的清单项应被识别");
    assert_eq!(r.done, 1);
}

/// **行号是 1 基**（与 D399 修掉的 `verify_chain` 0 基相反 —— 这里是**对的**）。
#[test]
fn d402_line_numbers_are_one_based() {
    let r = parse_heartbeat("# heading\n\n- [ ] first\n\n- [x] second\n", None);
    assert_eq!(r.items[0].line_number, 3, "首个清单项在第 3 个物理行");
    assert_eq!(r.items[1].line_number, 5);
}

/// **非清单行被跳过**且不计入 `total`。
#[test]
fn d402_non_item_lines_skipped() {
    let content = "# 标题\n\n普通段落\n* 用星号的列表项\n1. 有序列表\n- 不是复选框的行\n";
    let r = parse_heartbeat(content, None);
    assert_eq!(r.total, 0, "非 checklist 行不得计入");
    assert!(r.items.is_empty());
}

/// **`- [ ]` 后必须有空格**才被识别（markdown 规范如此）。
#[test]
fn d402_requires_space_after_bracket() {
    // 无空格：`- [x]x` 不是合法任务列表项
    let r = parse_heartbeat("- [x]tight\n", None);
    assert_eq!(r.total, 0, "`[x]` 后无空格不应被识别为清单项");
    // 有空格：识别
    let r2 = parse_heartbeat("- [x] spaced\n", None);
    assert_eq!(r2.total, 1);
}

/// **空清单**是 vacuously complete（与 `plan::completion_ratio` 同族语义）。
#[test]
fn d402_empty_is_vacuously_complete() {
    let r = parse_heartbeat("# only heading\n", None);
    assert_eq!(r.total, 0);
    assert!(r.is_complete());
    assert_eq!(r.completion_ratio(), 1.0);
    // 对照：与 plan 的空集合语义一致（兄弟不得分叉）
    let p = mora::plan::Plan::new();
    assert_eq!(
        p.completion_ratio(),
        1.0,
        "`plan.completion_ratio` 空时也应为 1.0 —— 两条独立的「完成度」语义须一致"
    );
}

/// **全 done** ⇒ complete，比例 1.0。
#[test]
fn d402_all_done_is_complete() {
    let r = parse_heartbeat("- [x] a\n- [X] b\n- [x] c\n", None);
    assert_eq!(r.done, 3);
    assert!(r.is_complete());
    assert_eq!(r.completion_ratio(), 1.0);
}

/// **混合比例**正确。
#[test]
fn d402_completion_ratio_correct() {
    let r = parse_heartbeat("- [x] a\n- [ ] b\n- [x] c\n- [ ] d\n", None);
    assert_eq!(r.completion_ratio(), 0.5);
}

/// **真实文件**加载路径可用，`source` 被记录。
#[test]
fn d402_load_real_file_records_source() {
    let dir = std::env::temp_dir().join("mora_d402_hb");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let path: PathBuf = dir.join("HEARTBEAT.md");
    std::fs::write(&path, "# H\n\n- [x] first\n- [ ] second\n").expect("写文件");

    let r = load_heartbeat(&path).expect("load 应成功");
    assert_eq!(r.total, 2);
    assert_eq!(r.done, 1);
    assert_eq!(r.source.as_deref(), Some(path.as_path()), "source 应被记录");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **不存在的文件**给出点名路径的错误。
#[test]
fn d402_missing_file_error_names_path() {
    let p = std::env::temp_dir().join("mora_d402_missing_hb.md");
    let _ = std::fs::remove_file(&p);
    let err = load_heartbeat(&p).expect_err("不存在的文件应报错");
    let msg = err.to_string();
    assert!(
        msg.contains("read ") && msg.contains("mora_d402_missing_hb.md"),
        "错误应点名动作与路径; 实得 {msg}"
    );
}

// ── ② 文档更正已落地 ──

/// **模块 doc 不得再声称存在 `heartbeat` 这个 builtin 模块**。
#[test]
fn d402_module_doc_no_longer_claims_heartbeat_check() {
    let doc = read("src/heartbeat/mod.rs");
    assert!(
        !doc.contains("builtin `heartbeat.check"),
        "`src/heartbeat/mod.rs` 仍在声称 builtin `heartbeat.check` —— \
         该名字不存在（实测 `Unbound variable 'heartbeat'`）"
    );
    assert!(
        doc.contains("ai.heartbeat"),
        "doc 应写明真实名字是 `ai.heartbeat`"
    );
    // 反向对照：真实实现确实在 `call_ai_method` 里
    let ai = read("src/interpreter/builtins/ai.rs");
    assert!(
        ai.contains("\"heartbeat\" =>"),
        "`call_ai_method` 里应仍有 `heartbeat` 分支"
    );
}

/// **文档承诺的拆雷信息必须在**：接线时要补 `check_path`。
///
/// ⚠ 首版只断言 doc 里**出现过** `check_path` 三个字，牙齿太弱 ——
/// 牙齿验证时我在探针里塞一行无关的 `check_path 拆雷说明` 就把它糊弄过去了。
/// 改为要求 `check_path` 与「接线」**在相邻行内成对出现**（结构性要求，
/// 不绑死具体措辞）。
#[test]
fn d402_doc_warns_about_sandbox_bypass() {
    let doc = read("src/heartbeat/mod.rs");
    let lines: Vec<&str> = doc.lines().map(str::trim).collect();
    let mut warned = false;
    for (i, l) in lines.iter().enumerate() {
        if !l.contains("check_path") {
            continue;
        }
        let lo = i.saturating_sub(6);
        let hi = (i + 7).min(lines.len());
        if lines[lo..hi].iter().any(|n| n.contains("接线")) {
            warned = true;
            break;
        }
    }
    assert!(
        warned,
        "doc 应在 `check_path` 附近说明「接线时必须补」—— \
         只出现 `check_path` 三个字不算数（首版牙齿太弱，已被验证抓出）"
    );
}

/// **`heartbeat` 不是 builtin 模块**（doc 不能凭空声称它存在）。
#[test]
fn d402_heartbeat_is_not_a_builtin_module() {
    let dispatch = read("src/typeck/dispatch.rs");
    // 模块分组表里不应出现 "heartbeat" 这一项
    assert!(
        !dispatch.contains("\"heartbeat\" =>"),
        "typeck 模块分组表里竟有 `heartbeat` 项 —— 前提变化，需重新评估"
    );
    // `ai` 的方法表里也**刻意不含** heartbeat（D59 收窄）
    let ai_methods = dispatch
        .split("const AI_MODULE_METHODS")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap_or("");
    assert!(
        !ai_methods.contains("heartbeat"),
        "`AI_MODULE_METHODS` 里不应列 `heartbeat`（源码不可达，列了会宣称调不通）"
    );
}

/// **`load_heartbeat` 的唯一调用方在 `ai.heartbeat` 分支里，而它不可达**。
///
/// ⚠ 我第一版断言「零生产调用方」**写错了**：实测 `builtins/ai.rs:198`
/// 确有 `crate::heartbeat::load_heartbeat(&path)`。
///
/// 准确事实分两层（D400 的教训：调用方**存在** ≠ 路径**可达**）：
/// - 调用方**存在**（`ai.rs` 的 `heartbeat` 分支）
/// - 但该分支挂 `BuiltinKind::Ai` 上，parser 永远给不出这个接收者
///   ⇒ **路径不可达**（D59）
#[test]
fn d402_load_heartbeat_only_called_from_the_unreachable_ai_branch() {
    let mut hits = Vec::new();
    for rel in [
        "src/builtins",
        "src/interpreter",
        "src/mir",
        "src/runtime",
        "src/cli",
    ] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        if !dir.exists() {
            continue;
        }
        walk(&dir, &mut hits);
    }
    let outside: Vec<String> = hits
        .iter()
        // ⚠ 归一化分隔符：命中文本是 `src/interpreter\builtins\ai.rs`
        //   （rel 用正斜杠、其后用反斜杠）—— 不归一化就会漏判。
        .map(|h| h.replace('\\', "/"))
        .filter(|h| !h.starts_with("src/interpreter/builtins/ai.rs"))
        .collect();
    assert!(
        outside.is_empty(),
        "`load_heartbeat` 出现了 `ai.heartbeat` **之外**的调用方 {outside:?} —— \
         沙箱拆雷的前提需重写：任何新调用方都**必须**同时补 `check_path`"
    );
    // 反向对照：调用方确实存在（否则上面那条就是「压根没人调」的空断言）
    assert!(
        !hits.is_empty(),
        "本应有 1 处 `load_heartbeat` 调用（ai.rs 的 heartbeat 分支）；实得 0 处"
    );
    // 且该分支所在的接收者不可达 ⇒ 沙箱拆雷的前提成立
    let dispatch = read("src/typeck/dispatch.rs");
    let ai_methods = dispatch
        .split("const AI_MODULE_METHODS")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap_or("");
    assert!(
        !ai_methods.contains("heartbeat"),
        "`AI_MODULE_METHODS` 不应列 `heartbeat` —— 若列了说明接线已完成，\
         本文件「拆雷」的前提需重写并补沙箱守卫的端到端判据"
    );
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            for (i, line) in src.lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                if t.contains("load_heartbeat(") && !t.contains("pub fn load_heartbeat") {
                    let rel = p
                        .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                        .unwrap_or(&p)
                        .display()
                        .to_string();
                    out.push(format!("{rel}:{}: {t}", i + 1));
                }
            }
        }
    }
}

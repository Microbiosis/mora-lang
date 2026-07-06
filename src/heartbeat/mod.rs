//! v0.47.0: Heartbeat executable checklist (mimiclaw §1.5 inspired)
//!
//! 灵感: mimiclaw `main/agent/heartbeat_service.c`
//! - mimiclaw 用 FreeRTOS timer 每 30min 触发, 读 `HEARTBEAT.md`
//! - HEARTBEAT.md 含 checklist (e.g. "- [ ] check memory")
//! - mimiclaw 把 md 文件作为可执行 agent 行为源
//!
//! v0.47.0 Mora adaptation:
//! - `HeartbeatChecklist` 解析 markdown 清单 (- [ ] / - [x])
//! - `HeartbeatReport` 返回 done/pending counts + items list
//! - builtin `heartbeat.check(path?)` -> Dict (报告) 或触发真实 action
//! - 真实**读取文件** (REAL file I/O, not metadata)

use std::path::{Path, PathBuf};

/// v0.47.0: 单个 checklist item
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatItem {
    pub text: String,
    pub done: bool,
    pub line_number: usize,
}

impl HeartbeatItem {
    pub fn parse(line: &str, line_number: usize) -> Option<Self> {
        let line = line.trim_start();
        if !line.starts_with("- ") {
            return None;
        }
        let rest = &line[2..];
        let (done, text) = if let Some(t) = rest.strip_prefix("[x] ") {
            (true, t.to_string())
        } else if let Some(t) = rest.strip_prefix("[X] ") {
            (true, t.to_string())
        } else if let Some(t) = rest.strip_prefix("[] ") {
            (false, t.to_string())
        } else if let Some(t) = rest.strip_prefix("[ ] ") {
            (false, t.to_string())
        } else {
            return None;
        };
        Some(HeartbeatItem {
            text,
            done,
            line_number,
        })
    }
}

/// v0.47.0: Heartbeat checklist report
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeartbeatReport {
    pub source: Option<PathBuf>,
    pub total: usize,
    pub done: usize,
    pub pending: usize,
    pub items: Vec<HeartbeatItem>,
}

impl HeartbeatReport {
    pub fn completion_ratio(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }

    pub fn is_complete(&self) -> bool {
        self.pending == 0
    }
}

/// v0.47.0: 解析 HEARTBEAT.md -> HeartbeatReport
pub fn parse_heartbeat(content: &str, source: Option<PathBuf>) -> HeartbeatReport {
    let mut report = HeartbeatReport {
        source,
        ..Default::default()
    };
    for (i, line) in content.lines().enumerate() {
        if let Some(item) = HeartbeatItem::parse(line, i + 1) {
            report.total += 1;
            if item.done {
                report.done += 1;
            } else {
                report.pending += 1;
            }
            report.items.push(item);
        }
    }
    report
}

/// v0.47.0: 从文件加载 + 解析
pub fn load_heartbeat(path: &Path) -> Result<HeartbeatReport, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    Ok(parse_heartbeat(&content, Some(path.to_path_buf())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_unchecked() {
        let item = HeartbeatItem::parse("- [ ] check memory", 1).unwrap();
        assert!(!item.done);
        assert_eq!(item.text, "check memory");
        assert_eq!(item.line_number, 1);
    }

    #[test]
    fn parse_checked() {
        let item = HeartbeatItem::parse("- [x] done task", 1).unwrap();
        assert!(item.done);
        assert_eq!(item.text, "done task");
    }

    #[test]
    fn parse_uppercase_x() {
        let item = HeartbeatItem::parse("- [X] uppercase", 1).unwrap();
        assert!(item.done);
    }

    #[test]
    fn parse_empty_checkbox() {
        let item = HeartbeatItem::parse("- [] empty check", 1).unwrap();
        assert!(!item.done);
    }

    #[test]
    fn parse_indented_checklist() {
        let item = HeartbeatItem::parse("  - [ ] indented", 5).unwrap();
        assert!(!item.done);
        assert_eq!(item.line_number, 5);
    }

    #[test]
    fn parse_non_checklist_returns_none() {
        assert!(HeartbeatItem::parse("regular text", 1).is_none());
        assert!(HeartbeatItem::parse("- regular item", 1).is_none());
        assert!(HeartbeatItem::parse("# heading", 1).is_none());
    }

    #[test]
    fn parse_full_heartbeat() {
        let content = r#"# Heartbeat Checklist

- [ ] check memory
- [x] check disk
- [ ] send report
  - [x] sub-task done
"#;
        let report = parse_heartbeat(content, None);
        assert_eq!(report.total, 4);
        assert_eq!(report.done, 2);
        assert_eq!(report.pending, 2);
        assert!(!report.is_complete());
    }

    #[test]
    fn empty_heartbeat_is_complete() {
        let report = parse_heartbeat("# only heading\n", None);
        assert_eq!(report.total, 0);
        assert!(report.is_complete()); // vacuously
        assert_eq!(report.completion_ratio(), 1.0);
    }

    #[test]
    fn all_done_is_complete() {
        let content = "- [x] a\n- [X] b\n- [x] c\n";
        let report = parse_heartbeat(content, None);
        assert_eq!(report.total, 3);
        assert_eq!(report.done, 3);
        assert!(report.is_complete());
    }

    #[test]
    fn completion_ratio_correct() {
        let content = "- [x] a\n- [ ] b\n- [x] c\n- [ ] d\n";
        let report = parse_heartbeat(content, None);
        assert_eq!(report.completion_ratio(), 0.5);
    }

    #[test]
    fn load_heartbeat_real_file() {
        let dir = std::env::temp_dir().join(format!(
            "mora_heartbeat_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("HEARTBEAT.md");
        let content = "# Heartbeat\n\n- [x] first done\n- [ ] second pending\n- [x] third done\n";
        std::fs::write(&path, content).unwrap();

        let report = load_heartbeat(&path).expect("load");
        assert_eq!(report.total, 3);
        assert_eq!(report.done, 2);
        assert_eq!(report.pending, 1);
        assert_eq!(
            report.source.as_ref().unwrap().to_str().unwrap(),
            path.to_str().unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::time::UNIX_EPOCH;
}

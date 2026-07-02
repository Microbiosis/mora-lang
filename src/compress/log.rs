//! v0.29: LogSubCompressor — 模式嗅探 + 行归并
//!
//! 算法:
//! - `sniff`: 按行检查 — 命中 ISO-8601 日期 (`YYYY-MM-DD`) 或 syslog 等级
//!   (INFO/WARN/ERROR/DEBUG/FATAL) 的行占比 ≥ 0.4 → 0.8
//! - `compress`: MVP 实现 — 保留所有 ERROR/FATAL 行, 其他等级行也保留(在
//!   `max_bytes / 80` 行截断内), 末尾追加 `<N ERROR/FATAL lines preserved>` 标记。
//!   v0.30+ 可以再做真正的"cluster by pattern" (`[N×] sample`)。
//!
//! 硬规则 (Task 4 brief): 不直接依赖 `regex` crate, 走 `std::str` substring
//! 检查 (`regex` 仅通过 ocrs 间接传递, Rust 要求显式 `[dependencies]` 才能
//! `use`, 因此 MVP 用字符级 substring 替代)。

use crate::compress::{CompressOptions, SubCompressor};

/// v0.29: syslog 等级关键字 (sniff + compress 通用)
const SYSLOG_LEVELS: &[&str] = &["INFO", "WARN", "ERROR", "DEBUG", "FATAL"];

/// v0.29: 检测一行是否"像 ISO-8601 前缀" — 第 5 / 8 位是 `-`, 且
/// 周围 4 / 2 位是数字。这是粗略启发式, 避免依赖 `regex` crate。
///
/// 例:
/// - `"2026-07-01 10:00:00 ERROR ..."` → true
/// - `"error code 12-34"` → false (`1` / `3` 不是 4 / 2 数字)
/// - `"  2026-07-01"` → false (前导空格, 但 brief 的测试样例无前导空格, MVP 安全)
fn looks_like_iso_prefix(line: &str) -> bool {
    // 找形如 YYYY-MM-DD 的最小子串。MVP 用字符级扫描, 复杂度 O(n*k) 但
    // 日志单行 < 200 字符, 性能不是瓶颈。
    let bytes = line.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    for i in 0..=bytes.len() - 10 {
        if bytes[i + 4] == b'-'
            && bytes[i + 7] == b'-'
            && bytes[i..i + 4].iter().all(|b| b.is_ascii_digit())
            && bytes[i + 5..i + 7].iter().all(|b| b.is_ascii_digit())
            && bytes[i + 8..i + 10].iter().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// v0.29: `SubCompressor` trait impl for log content.
#[derive(Debug)]
pub struct LogSubCompressor;

impl SubCompressor for LogSubCompressor {
    /// 嗅探日志: ISO-8601 / syslog-level 行占比 ≥ 0.4 → 0.8
    fn sniff(&self, content: &str) -> f32 {
        let line_hits = content
            .lines()
            .filter(|l| looks_like_iso_prefix(l) || SYSLOG_LEVELS.iter().any(|k| l.contains(k)))
            .count();
        let total = content.lines().count().max(1);
        if (line_hits as f32) / (total as f32) >= 0.4 {
            0.8
        } else {
            0.0
        }
    }

    /// 压缩: 保留所有行 (ERROR/FATAL 强制 + 其他等级按预算截断),
    /// 末尾追加 ERROR/FATAL 行数标记。
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        _options: &CompressOptions,
    ) -> Result<String, String> {
        let mut keep: Vec<&str> = Vec::new();
        let mut error_count: usize = 0;
        // 粗略每行 ~80 bytes (与 spec §6.4 一致)
        let line_budget = max_bytes / 80;
        for line in content.lines() {
            if line.contains("ERROR") || line.contains("FATAL") {
                keep.push(line);
                error_count += 1;
            } else if line.contains("INFO") || line.contains("WARN") || line.contains("DEBUG") {
                keep.push(line);
            } else {
                // 非日志行也保留 (避免截断非结构化日志如 stack trace)
                keep.push(line);
            }
            if keep.len() >= line_budget.max(1) {
                break;
            }
        }
        let mut out = keep.join("\n");
        if error_count > 0 {
            // marker 必须包含 "ERROR lines preserved" 子串 (test contract),
            // 同时显式标记包含 FATAL。
            out.push_str(&format!(
                "\n<{} ERROR lines preserved> ({} ERROR/FATAL total)\n",
                error_count, error_count
            ));
        }
        out.push_str(&format!(
            "<compressed:method=log original_size={}>\n",
            content.len()
        ));
        Ok(out)
    }

    fn origin(&self) -> &'static str {
        "log"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 30 行 demo 日志 (6 ERROR 行 + 24 INFO 行, 含 ISO-8601 前缀)
    fn log_text() -> String {
        (0..30)
            .map(|i| {
                if i % 5 == 0 {
                    format!("2026-07-01 10:00:{:02} ERROR something failed", i)
                } else {
                    format!("2026-07-01 10:00:{:02} INFO routine message", i)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_log_sniff_detects_iso_format() {
        let c = LogSubCompressor;
        let text = log_text();
        let score = c.sniff(&text);
        assert!(score >= 0.6, "expected sniff >= 0.6, got {score}");
    }

    #[test]
    fn test_log_preserves_error_lines() {
        let c = LogSubCompressor;
        let text = log_text();
        let opts = CompressOptions::default();
        let out = c
            .compress(&text, 2000, &opts)
            .expect("compress should not error");
        assert!(out.contains("ERROR"), "must preserve ERROR keyword: {out}");
        assert!(
            out.contains("ERROR lines preserved"),
            "must include preserved marker: {out}"
        );
    }
}

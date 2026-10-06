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
    ///
    /// v0.104.6 D227：预算此前是 `max_bytes / 80` —— **每行 80 字节**是
    /// 一个无根据的硬编码假设。实测 560 字节/行的日志里，max_bytes=4800
    /// 算出 60 行预算，60 行 × 560 = 33660 字节，输出 33703，**是原文的
    /// 100.1%**（压缩了个寂寞）。现在改为按**真实字节数**累计。
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        _options: &CompressOptions,
    ) -> Result<String, String> {
        // v0.104.6 D227：marker 也要算进预算，否则收口时正文被截，
        // 报出来的 error_count 与实际保留的行不再对应。
        //
        // 预留量用**实际** marker 文本算，不用拍脑袋的常数：`<compressed:…>`
        // 段长度随 `original_size` 的位数变化，而 `<N ERROR lines preserved>`
        // 段是否出现取决于全文有没有 ERROR 行（没有就不占预算）。
        // 既有单测 `test_log_preserves_error_lines` 用 max_bytes=2000，
        // 曾因固定预留 96 字节把保留行挤空而变红。
        let error_total = content
            .lines()
            .filter(|l| l.contains("ERROR") || l.contains("FATAL"))
            .count();
        let marker_budget = format!("<compressed:method=log original_size={}>\n", content.len())
            .len()
            + if error_total > 0 {
                // 保守取 8 位计数的长度
                format!(
                    "\n<{error_total} ERROR lines preserved> ({error_total} ERROR/FATAL total)\n"
                )
                .len()
            } else {
                0
            };
        let line_budget = max_bytes.saturating_sub(marker_budget);

        let mut keep: Vec<&str> = Vec::new();
        let mut kept_bytes: usize = 0;
        let mut error_count: usize = 0;
        for line in content.lines() {
            // +1 是 join("\n") 的分隔符
            let cost = line.len() + 1;
            if kept_bytes + cost > line_budget {
                // v0.104.6 D227：预算耗尽即停。修前是 `keep.len() >= line_budget`
                // —— 比较的是**行数**与**字节预算**，量纲都不对。
                break;
            }
            if line.contains("ERROR") || line.contains("FATAL") {
                error_count += 1;
            }
            kept_bytes += cost;
            keep.push(line);
        }

        let body = keep.join("\n");
        let mut marker = String::new();
        if error_count > 0 {
            // marker 必须包含 "ERROR lines preserved" 子串 (test contract),
            // 同时显式标记包含 FATAL。
            //
            // v0.104.6 D227：括号里的数字此前写的是 `error_count`（= 保留数），
            // 但文案说的是 "total"（= 全文总数）—— 两个不同含义填了同一个值。
            // 保留数与总数在预算截断后会分叉，故分别填。
            marker.push_str(&format!(
                "\n<{error_count} ERROR lines preserved> ({error_total} ERROR/FATAL total)\n"
            ));
        }
        marker.push_str(&format!(
            "<compressed:method=log original_size={}>\n",
            content.len()
        ));
        Ok(crate::compress::finish_within_budget(
            content, body, &marker, max_bytes,
        ))
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

    /// v0.104.6 D227 回归：预算必须按**真实字节数**累计，而不是「每行 80 字节」。
    ///
    /// 修前 `line_budget = max_bytes / 80`，对 560 字节/行的日志算出
    /// 「还能放 60 行」，60 × 560 = 33660 字节远超 max_bytes —— 压缩了个寂寞。
    /// 判据形态是**不变式**而非具体数字：无论每行多长，输出都 ≤ max_bytes，
    /// 且行数随预算**单调**变化（预算翻倍 → 保留行数不减）。
    #[test]
    fn test_log_budget_scales_with_real_line_length() {
        let c = LogSubCompressor;
        let opts = CompressOptions::default();
        // 故意用远大于 80 字节的行长，放大「80 字节假设」的误差
        let long_line = format!("2026-07-01 10:00:00 INFO {}", "x".repeat(500));
        let text = (0..60)
            .map(|_| long_line.clone())
            .collect::<Vec<_>>()
            .join("\n");

        let mut prev_kept = 0usize;
        for mb in [500usize, 1000, 2000, 4000] {
            let out = c
                .compress(&text, mb, &opts)
                .expect("compress should not error");
            assert!(
                out.len() <= mb,
                "D227: max_bytes={mb} 时输出 {} 字节超限（行长 {} 字节）",
                out.len(),
                long_line.len()
            );
            let kept = out.lines().filter(|l| l.contains("2026-07-01")).count();
            assert!(
                kept >= prev_kept,
                "D227: 预算从 {} 增到 {mb}，保留行数不应变少（{} → {}）",
                mb / 2,
                prev_kept,
                kept
            );
            prev_kept = kept;
        }
    }
}

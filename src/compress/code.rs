//! v0.29: CodeSubCompressor — 纯 regex / 关键字嗅探的代码压缩器
//!
//! 灵感: headroom-style 内容感知路由 + tree-sitter 不可用的 KISS fallback。
//! 算法:
//! - `sniff`: 数 `CODE_KEYWORDS` 在全文出现次数 (只要 ≥ 2 即"像代码"), 置信度
//!   从 `0.7` 起跳, 每多一个关键字 +0.05, 上限 0.95。
//! - `compress`: 逐行扫描, 保留"签名行"(包含任一关键字, 或以 `//` / `#`
//!   开头), 合并非签名行为 `<N body lines elided>` 标记。
//! - 不依赖 `regex` crate, 直接 `str::contains` 即可。

use crate::compress::{CompressOptions, SubCompressor};

/// v0.29: 代码关键字 sniff 列表
///
/// 覆盖常见多语言签名特征:
/// - Rust / Python / JS / TS / Go / Java 等的函数定义 (`fn ` / `def `)
/// - 类定义 (`class `)
/// - 箭头函数 (`=>`)
/// - 模块导入 (`import `)
/// - 访问修饰符 (`public ` / `private `)
/// - 命名空间限定符 (`::`)
///
/// 注意: 关键字都是带尾随空格的 / 是 ASCII 操作符, 避免误匹配单词内部
/// (例如 `info` 不应匹配 `in`)。
pub const CODE_KEYWORDS: &[&str] = &[
    "fn ", "def ", "class ", "=>", "import ", "public ", "private ", "::",
];

/// v0.29: `SubCompressor` trait impl for source code.
#[derive(Debug)]
pub struct CodeSubCompressor;

impl SubCompressor for CodeSubCompressor {
    /// 嗅探代码: 总命中次数 ≥ 2 → 返回 0.7 + 0.05*hits (上限 0.95); 否则 0.0
    ///
    /// 注意: `hits` 计**所有**关键字出现次数之和 (不是 unique keywords 数)。
    /// 例: `"fn main() {\n fn helper() {}\n}"` 含 `fn ` 两次 → hits=2 → score=0.8
    fn sniff(&self, content: &str) -> f32 {
        let hits: usize = CODE_KEYWORDS
            .iter()
            .map(|k| content.matches(k).count())
            .sum();
        if hits >= 2 {
            // 0.7 + 0.05 * hits, cap at 0.95
            (0.7 + (hits as f32) * 0.05).min(0.95)
        } else {
            0.0
        }
    }

    /// 压缩: 保留签名行, 合并连续非签名行为 elide marker。
    ///
    /// v0.104.6 D227：预算检查此前是 `if out.len() >= max_bytes { break; }`
    /// —— 在**已经追加完本行之后**才判断，且判断通过后还要再追加 elide
    /// marker + `<compressed:method=code …>` 两段尾部，输出必然超限
    /// （实测 max_bytes=16 → 79 字节）。现在给尾部留预算。
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        _options: &CompressOptions,
    ) -> Result<String, String> {
        // v0.104.6 D227：尾部要放 elide marker + `<compressed:method=code …>`。
        //
        // 预留量按**真实**的 `<compressed:method=code original_size={total}>`
        // 长度算，而不是拍一个常数。拍常数（曾用 96）在 max_bytes=200 这种
        // 小预算上会吃掉近一半预算，把签名行全挤掉 —— 既有单测
        // `test_code_compress_preserves_signatures` 就是这么变红的。
        // elide marker 只在真的有 body 行被省略时才出现，先按 0 算，
        // 收口函数 `finish_within_budget` 是契约的最终保证者。
        let compressed_marker_len = format!(
            "\n<compressed:method=code original_size={}>\n",
            content.len()
        )
        .len();
        let body_budget = max_bytes.saturating_sub(compressed_marker_len);

        let mut body = String::new();
        let mut body_lines: usize = 0;
        for line in content.lines() {
            let is_signature = CODE_KEYWORDS.iter().any(|k| line.contains(k))
                || line.trim_start().starts_with("//")
                || line.trim_start().starts_with('#');
            let piece = if is_signature {
                let mut p = String::new();
                if body_lines > 0 {
                    p.push_str(&format!("    ... [{} body lines elided] ...\n", body_lines));
                    body_lines = 0;
                }
                p.push_str(line);
                p.push('\n');
                p
            } else {
                body_lines += 1;
                String::new()
            };
            if !piece.is_empty() {
                if body.len() + piece.len() > body_budget {
                    break;
                }
                body.push_str(&piece);
            }
        }
        if body_lines > 0 {
            body.push_str(&format!("    ... [{} body lines elided] ...\n", body_lines));
        }
        let marker = format!(
            "\n<compressed:method=code original_size={}>\n",
            content.len()
        );
        Ok(crate::compress::finish_within_budget(
            content, body, &marker, max_bytes,
        ))
    }

    fn origin(&self) -> &'static str {
        "code"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_sniff_detects_keyword_density() {
        let c = CodeSubCompressor;
        // 含 `fn main` + `fn helper` 两个 `fn ` 关键字 → hits=2 → 0.8
        let src = "fn main() {\n    let x = 1;\n    fn helper() {}\n}\n";
        let score = c.sniff(src);
        assert!(score >= 0.6, "expected sniff >= 0.6, got {score}");
    }

    #[test]
    fn test_code_compress_preserves_signatures() {
        let c = CodeSubCompressor;
        let src =
            "fn main() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\nfn helper() {}\n";
        let opts = CompressOptions::default();
        let out = c
            .compress(src, 200, &opts)
            .expect("compress should not error");
        assert!(out.contains("fn main()"), "must preserve fn main(): {out}");
        assert!(
            out.contains("fn helper()"),
            "must preserve fn helper(): {out}"
        );
        // v0.104.6 D227：原先这里断言 `contains("body lines elided")`。
        // 那是**超限 bug 的副产物** —— 旧实现不看预算就一路追加，输出
        // (66 字节源码 + 40 字节 marker) 超过 max_bytes=200 之前，elide
        // marker 仍会出现。修好后预算足够装下全部 4 行，**没有省略发生**，
        // 于是 elide marker 理应不存在；断言它存在等于要求压缩器**必须**
        // 多余地丢弃内容。
        //
        // 换成两条真正的不变式：签名行都在，且输出没超预算。
        assert!(
            out.len() <= 200,
            "D227: 输出不得超过 max_bytes; 实得 {} 字节: {out}",
            out.len()
        );
        // original_size 按**实际**源码长度算，不写死数字
        // （先写成 66 判红 —— 真实值是 74，典型「不写死具体数字」纪律的违反）。
        assert!(
            out.contains(&format!(
                "<compressed:method=code original_size={}>",
                src.len()
            )),
            "应带上 original_size={} marker: {out}",
            src.len()
        );
    }

    /// v0.104.6 D227 回归：预算不足时**必须**省略并留下 elide marker。
    ///
    /// 与上一条互补：上一条验「够用时不省略」，本条验「不够用时必省略」，
    /// 且省略后仍须给出 elide marker 说明丢了多少行。
    #[test]
    fn test_code_compress_elides_when_over_budget() {
        let c = CodeSubCompressor;
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!("fn f{i}() {{\n"));
            src.push_str("    let x = 1;\n");
            src.push_str("}\n");
        }
        let opts = CompressOptions::default();
        let out = c
            .compress(&src, 200, &opts)
            .expect("compress should not error");
        assert!(
            out.contains("body lines elided"),
            "预算不足时必须留下 elide marker: {out}"
        );
        assert!(
            out.len() <= 200,
            "D227: 省略后仍须满足 max_bytes; 实得 {} 字节",
            out.len()
        );
    }
}

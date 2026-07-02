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
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        _options: &CompressOptions,
    ) -> Result<String, String> {
        let mut out = String::new();
        let mut body_lines: usize = 0;
        for line in content.lines() {
            let is_signature = CODE_KEYWORDS.iter().any(|k| line.contains(k))
                || line.trim_start().starts_with("//")
                || line.trim_start().starts_with('#');
            if is_signature {
                if body_lines > 0 {
                    out.push_str(&format!("    ... [{} body lines elided] ...\n", body_lines));
                    body_lines = 0;
                }
                out.push_str(line);
                out.push('\n');
            } else {
                body_lines += 1;
            }
            if out.len() >= max_bytes {
                break;
            }
        }
        if body_lines > 0 {
            out.push_str(&format!("    ... [{} body lines elided] ...\n", body_lines));
        }
        out.push_str(&format!(
            "\n<compressed:method=code original_size={}>\n",
            content.len()
        ));
        Ok(out)
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
        assert!(
            out.contains("body lines elided"),
            "must include elide marker: {out}"
        );
    }
}

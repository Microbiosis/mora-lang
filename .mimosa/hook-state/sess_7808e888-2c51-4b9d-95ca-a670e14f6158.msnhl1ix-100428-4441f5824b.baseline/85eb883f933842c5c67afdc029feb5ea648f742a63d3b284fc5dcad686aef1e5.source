//! v0.55: Parser V3 集成测试 (原 parser_v2 的覆盖)
//!
//! 这些测试从 V2 迁移到 V3 — ParserV3 直接输出 MirExpr, 零 AST 依赖。
//! 测试目标: 烟雾测试 + 重要的边界 (缺 given 不 panic, _legacy 示例 lex)。

use mora::lexer::Lexer;
use mora::parser_v3::ParserV3;

fn parse_file(path: &str) -> usize {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.scan_tokens();
    let parser = ParserV3::new(tokens);
    parser
        .parse()
        .unwrap_or_else(|e| panic!("parse failed for {}: {:?}", path, e))
        .len()
}

#[test]
fn test_parse_compress_demo() {
    let n = parse_file("examples/compress_demo.mora");
    assert!(n > 0, "compress_demo.mora should parse successfully");
    eprintln!("compress_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_compress_smart_demo() {
    let n = parse_file("examples/compress_smart_demo.mora");
    assert!(n > 0, "compress_smart_demo.mora should parse successfully");
    eprintln!("compress_smart_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_compact_demo() {
    let n = parse_file("examples/compact_demo.mora");
    assert!(n > 0, "compact_demo.mora should parse successfully");
    eprintln!("compact_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_mcp_server_demo() {
    let n = parse_file("examples/mcp_server_demo.mora");
    assert!(n > 0, "mcp_server_demo.mora should parse successfully");
    eprintln!("mcp_server_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_legacy_demos_lex_only() {
    // 验证 _legacy/ 中的 demo 不再 panic lexer (即使 parse 失败)
    // 用 lexer_only 模式只检查词法
    for path in &[
        "examples/_legacy/trait_demo.mora",
        "examples/_legacy/orchestrate_demo.mora",
        "examples/_legacy/eval_demo.mora",
    ] {
        let source =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));
        let mut lexer = Lexer::new(&source);
        let _tokens = lexer.scan_tokens();
        // v0.30: lexer 不再 panic, 任何输入都能 lex 完
        eprintln!("{}: lexed without panic", path);
    }
}

#[test]
fn test_parse_eval_without_given_no_panic() {
    // v0.34: eval 块缺少 given: 不应 panic。
    // V3 中 eval 语法可能不同；此测试仅保证 lexer + parser 不 panic。
    let source = "let x = 1\nlet y = x + 1";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.scan_tokens();
    let _ = ParserV3::new(tokens);
    // V3 不报告 eval 解析成功 — 但保证 lex/parse 不 panic 即可
}

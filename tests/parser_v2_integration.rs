use mora::lexer::Lexer;
use mora::parser_v2::ParserV2;

fn parse_file(path: &str) -> usize {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.scan_tokens();
    let mut parser = ParserV2::new(tokens);
    let stmts = parser.parse();
    stmts.len()
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
    use mora::lexer::Lexer;
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

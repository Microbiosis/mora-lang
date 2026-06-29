use mora::lexer::Lexer;
use mora::parser_v2::ParserV2;

fn parse_file(path: &str) -> usize {
    let source = std::fs::read_to_string(path).unwrap();
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.scan_tokens();
    let mut parser = ParserV2::new(tokens);
    let stmts = parser.parse();
    stmts.len()
}

#[test]
fn test_parse_trait_demo() {
    let n = parse_file("examples/trait_demo.mora");
    assert!(n > 0, "trait_demo.mora should parse successfully");
    eprintln!("trait_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_container() {
    let n = parse_file("examples/container.mora");
    assert!(n > 0, "container.mora should parse successfully");
    eprintln!("container.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_observe_demo() {
    let n = parse_file("examples/observe_demo.mora");
    assert!(n > 0, "observe_demo.mora should parse successfully");
    eprintln!("observe_demo.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_generic_with_where() {
    let n = parse_file("examples/generic_with_where.mora");
    assert!(n > 0, "generic_with_where.mora should parse successfully");
    eprintln!("generic_with_where.mora: {} top-level nodes", n);
}

#[test]
fn test_parse_nested_generic() {
    let n = parse_file("examples/nested_generic.mora");
    assert!(n > 0, "nested_generic.mora should parse successfully");
    eprintln!("nested_generic.mora: {} top-level nodes", n);
}

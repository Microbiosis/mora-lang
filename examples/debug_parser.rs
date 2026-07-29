// Diagnostic: show parse errors instead of swallowing them
use mora::lexer::Lexer;
use mora::parser_v3::ParserV3;

fn main() {
    let src = r#"
task main()
  let x = 1
  print(x)
end
"#;
    
    println!("Running parser on simple source...");
    let mut lexer = Lexer::new(src);
    let tokens = lexer.scan_tokens();
    println!("Lexer produced {} tokens", tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        println!("  token[{}]: {:?}", i, t.token_type);
    }
    
    match ParserV3::new(tokens).parse() {
        Ok(exprs) => {
            println!("Parser returned {} expressions", exprs.len());
            for (i, e) in exprs.iter().enumerate() {
                println!("  expr[{}]: {:?}", i, e.kind);
            }
        }
        Err(e) => {
            println!("Parser ERROR: {}", e.0);
        }
    }
}

use mora::lexer::Lexer;
use mora::parser_v3::ParserV3;

#[test]
fn test_token_stream_inline_closure() {
    let src = "fn(f) f(21)";
    let tokens = Lexer::new(src).scan_tokens();
    for (i, t) in tokens.iter().enumerate() {
        println!(
            "[{} token={:?} line={} col={}",
            i, t.token_type, t.line, t.column
        );
    }

    match ParserV3::compile(src) {
        Ok((func, _)) => {
            println!("SUCCESS: {} insts", func.body.len());
            for (i, inst) in func.body.iter().enumerate() {
                println!("  [{}] {:?}", i, inst);
            }
        }
        Err(e) => {
            println!("FAILED: {}", e);
        }
    }
}

#[test]
fn test_direct_compile_closure() {
    let src = "fn(f) f(21)";
    println!("Trying to compile: {:?}", src);
    let result = ParserV3::compile(src);
    match &result {
        Ok((func, _)) => {
            println!("OK: {} insts, n_regs={}", func.body.len(), func.n_regs);
            for (i, inst) in func.body.iter().enumerate() {
                println!("  [{}] {:?}", i, inst);
            }
        }
        Err(e) => {
            println!("ERR: {}", e);
        }
    }
    assert!(
        result.is_ok(),
        "closure 'fn(f) f(21)' should compile but got error: {:?}",
        result.err()
    );
}

#[test]
fn test_compile_closure_with_end() {
    let src = "fn(f)\n  f(21)\nend";
    let result = ParserV3::compile(src);
    match &result {
        Ok((func, _)) => {
            println!("OK: {} insts", func.body.len());
            for (i, inst) in func.body.iter().enumerate() {
                println!("  [{}] {:?}", i, inst);
            }
        }
        Err(e) => {
            println!("ERR: {}", e);
        }
    }
    assert!(result.is_ok());
}

#[test]
fn test_compile_closure_with_braces() {
    let src = "fn(f) { f(21) }";
    let result = ParserV3::compile(src);
    match &result {
        Ok((func, _)) => {
            println!("OK: {} insts", func.body.len());
            for (i, inst) in func.body.iter().enumerate() {
                println!("  [{}] {:?}", i, inst);
            }
        }
        Err(e) => {
            println!("ERR: {}", e);
        }
    }
    assert!(result.is_ok());
}

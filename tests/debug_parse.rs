// Minimal debug test - only syntax ParserV3 definitely supports
use mora::interpreter::parse_code_v3;
use mora::mir::lower::lower_mir_exprs;

#[test]
fn debug_minimal() {
    // Use simple let bindings that ParserV3 supports (task syntax not yet in V3)
    let src = r#"let x = 1
let y = x + 2
print(y)"#;

    eprintln!("=== MINIMAL: parse start ===");
    let exprs = parse_code_v3(src).expect("parse should succeed");
    eprintln!("=== MINIMAL: parse done, {} expressions ===", exprs.len());
    for (i, e) in exprs.iter().enumerate() {
        eprintln!("  expr[{}]: {:?}", i, e.kind);
    }

    let result = lower_mir_exprs(&exprs);
    eprintln!("=== MINIMAL: lower done, ok={} ===", result.is_ok());
    if let Ok(func) = &result {
        eprintln!("n_regs={}, body.len={}", func.n_regs, func.body.len());
        for (i, inst) in func.body.iter().enumerate() {
            eprintln!("  inst[{}]: {:?}", i, inst);
        }
    }
    assert!(result.is_ok(), "lowering should succeed");
}

// Debug test to see what ParserV3 produces for various inputs
use mora::interpreter::parse_code_v3;
use mora::mir::lower::lower_mir_exprs;

fn main() {
    // Test 1: Simple if without else (from typeck_passes_then_mir_runs)
    let src1 = r#"
type Bytes = number

task main()
  let n = 5
  let label = "zero"
  if n > 0 then
    let label = "positive"
  end
  if n < 0 then
    let label = "negative"
  end
  print(label)
end
"#;
    
    println!("=== Test 1: typeck_passes_then_mir_runs ===");
    let exprs = parse_code_v3(src1).expect("parse should succeed");
    println!("Parsed {} expressions", exprs.len());
    for (i, e) in exprs.iter().enumerate() {
        println!("  expr[{}]: {:?}", i, e.kind);
    }
    match lower_mir_exprs(&exprs) {
        Ok(func) => {
            println!("Lowering OK: n_regs={}, body.len={}", func.n_regs, func.body.len());
            for (i, inst) in func.body.iter().enumerate() {
                println!("  inst[{}]: {:?}", i, inst);
            }
        }
        Err(e) => println!("Lowering ERROR: {}", e),
    }
    
    // Test 2: Transaction
    let src2 = r#"
task main()
  transaction
    let x = 1 + 1
    print("ok=" + x)
  compensation
    print("never")
  end
end
"#;
    
    println!("\n=== Test 2: transaction success ===");
    let exprs2 = parse_code_v3(src2).expect("parse should succeed");
    println!("Parsed {} expressions", exprs2.len());
    for (i, e) in exprs2.iter().enumerate() {
        println!("  expr[{}]: {:?}", i, e.kind);
    }
    
    // Test 3: Simple working case
    let src3 = r#"
task main()
  let greeting = "Hello"
  print(greeting)
end
"#;
    
    println!("\n=== Test 3: simple working case ===");
    let exprs3 = parse_code_v3(src3).expect("parse should succeed");
    println!("Parsed {} expressions", exprs3.len());
    for (i, e) in exprs3.iter().enumerate() {
        println!("  expr[{}]: {:?}", i, e.kind);
    }
    match lower_mir_exprs(&exprs3) {
        Ok(func) => {
            println!("Lowering OK: n_regs={}, body.len={}", func.n_regs, func.body.len());
            for (i, inst) in func.body.iter().enumerate() {
                println!("  inst[{}]: {:?}", i, inst);
            }
        }
        Err(e) => println!("Lowering ERROR: {}", e),
    }
}

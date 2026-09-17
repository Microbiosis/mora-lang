use mora::parser_v3::ParserV3;

fn main() {
    let src = "let f = fn(x) x * 2 end\nprint(f(5))";
    println!("Compiling...");
    let (_func, witnesses) = ParserV3::compile(src).expect("compile should succeed");
    println!("Compiled OK, {} witnesses", witnesses.len());
    for (i, w) in witnesses.iter().enumerate() {
        println!("  Witness {}: kind={:?}", i, w.kind);
    }
    println!("\nChecking types...");
    std::io::Write::flush(&mut std::io::stdout()).expect("stdout flush failed");
    let type_errors = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if type_errors.is_empty() {
        println!("typeck OK");
    } else {
        for e in &type_errors {
            println!("  type error: {:?}", e);
        }
    }
}

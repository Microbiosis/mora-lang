use mora::mir::lower::lower_mir_exprs;
use mora::mir::ssa::{construct, deconstruct};

#[test]
fn debug_mir_dump() {
    let exprs = mora::interpreter::parse_code_v3("let x = 1 + 2").expect("parse failed");
    let orig = lower_mir_exprs(&exprs).expect("lowering failed");
    println!("=== MIR (n_regs={}) ===", orig.n_regs);
    for (i, inst) in orig.body.iter().enumerate() {
        println!("  {}: {:?}", i, inst);
    }

    let ssa = construct(&orig);
    println!("\n=== SSA ({} blocks) ===", ssa.blocks.len());
    for block in &ssa.blocks {
        println!("Block {}:", block.id);
        println!("  preds: {:?}  succs: {:?}", block.preds, block.succs);
        println!("  terminator: {:?}", block.terminator);
        for inst in &block.insts {
            println!("  inst: {:?}", inst);
        }
    }

    let new = deconstruct(&ssa);
    println!("\n=== Deconstructed MIR (n_regs={}) ===", new.n_regs);
    for (i, inst) in new.body.iter().enumerate() {
        println!("  {}: {:?}", i, inst);
    }
}

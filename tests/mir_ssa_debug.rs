use mora::mir::lower::lower_program;
use mora::mir::ssa::{construct, deconstruct};

#[test]
fn debug_mir_dump() {
    let (node_ids, arena) = mora::interpreter::parse_code("let x = 1 + 2");
    let orig = lower_program(&node_ids, &arena).expect("lowering failed");
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

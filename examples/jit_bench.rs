//! v0.75.46: copy-and-patch JIT vs MIR 解释器 benchmark（人工观测）。
//!
//! 同一纯算术函数（1M 次执行）：JIT（原生 SSE2 代码）vs 解释器
//! （DAG 波次调度）。结果不落测试断言（CI 环境抖动），仅打印耗时供
//! 人工对比 —— JIT 应显著快于解释器（指令直落 vs 逐节点 dispatch）。
//! 运行：cargo run --release --example jit_bench

use mora::mir::{MirFunction, MirInst};
use mora::value::Value;
use std::time::Instant;

/// 纯算术函数：`(a + b) * c / d`（Float 可编译子集），1M 次执行。
fn bench_func() -> MirFunction {
    MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Float(10.0)),
            MirInst::Const(1, Value::Float(4.0)),
            MirInst::Const(2, Value::Float(2.0)),
            MirInst::Const(3, Value::Float(3.0)),
            MirInst::BinaryOp(4, 0, mora::common::BinaryOp::Add, 1), // 14
            MirInst::BinaryOp(5, 4, mora::common::BinaryOp::Mul, 2), // 28
            MirInst::BinaryOp(6, 5, mora::common::BinaryOp::Div, 3), // 9.33
        ],
        n_regs: 7,
        ..Default::default()
    }
}

fn main() {
    let func = bench_func();
    const N: u32 = 1_000_000;

    // JIT 预热（首次编译含可执行内存分配）
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let _ = mora::mir::jit::run_jit(&func, &mut interp, &mut env).expect("JIT should compile");

    let start = Instant::now();
    for _ in 0..N {
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let _ = mora::mir::jit::run_jit(&func, &mut interp, &mut env).unwrap();
    }
    let jit_elapsed = start.elapsed();

    let start = Instant::now();
    for _ in 0..N {
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let _ = mora::mir::vm::run_mir(&std::sync::Arc::new(func.clone()), &mut interp, &mut env)
            .unwrap();
    }
    let mir_elapsed = start.elapsed();

    println!("=== copy-and-patch JIT vs MIR interp ({N} executions) ===");
    println!("JIT : {jit_elapsed:?}");
    println!("MIR : {mir_elapsed:?}");
    let speedup = mir_elapsed.as_nanos() as f64 / jit_elapsed.as_nanos().max(1) as f64;
    println!("speedup: {speedup:.2}x");
}

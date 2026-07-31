//! α.8: JIT 编译器 — SSA → LLVM IR → native
//!
//! 编译门控：`#[cfg(feature = "jit")]`，无 LLVM 时跳过编译。
//! 实际 LLVM 绑定（inkwell）通过 Cargo feature 启用。
//!
//! 流程：
//! 1. SSA → LLVM IR（逐块翻译）
//! 2. LLVM 优化（O3）
//! 3. JIT 编译 → native code
//! 4. 调用生成的函数
//!
//! TODO: 实际 inkwell 绑定需要 LLVM 系统库

use crate::mir::ssa::{MirSsaFunction, RegType};
use crate::value::{Environment, Value};

/// α.8: JIT 编译并执行 SSA 函数
///
/// 当前阶段：标记位已传递，实际 JIT 编译需 LLVM 系统库。
/// 无 LLVM 时 panic with clear error。
///
/// 设计：SSA 寄存器是无类型的（usize 索引），LLVM IR 需要具体类型。
/// 翻译前必须先运行 typeinfer 填充 ssa.types。
#[cfg(feature = "jit")]
pub fn run_jit(
    _ssa: &MirSsaFunction,
    _interp: &mut dyn crate::mir::host::MirHost,
    _env: &mut Environment,
) -> Result<Value, String> {
    // TODO: 实现实际 SSA → LLVM IR → JIT 编译
    //
    // 需要 inkwell 绑定：
    // let context = inkwell::context::Context::new();
    // let module = context.create_module("mora_jit");
    // let builder = context.create_builder();
    //
    // for block in &ssa.blocks {
    //     let llvm_block = module.append_basic_block(format!("block_{}", block.id));
    //     builder.position_at_end(llvm_block);
    //     // 翻译 phi nodes
    //     // 翻译 instructions
    //     // 翻译 terminator
    // }
    //
    // let jit = module.create_jit_compiler()?;
    // let fn_ptr = jit.get_function::<fn() -> f64>("main")?;
    // Ok(Value::Float(unsafe { fn_ptr() }))

    Err(
        "JIT compiler requires LLVM system library (enable 'jit' feature with LLVM 17 installed)"
            .to_string(),
    )
}

/// 无 LLVM 时的 stub：始终返回 MIR 解释器
#[cfg(not(feature = "jit"))]
pub fn run_jit(
    _ssa: &MirSsaFunction,
    _interp: &mut dyn crate::mir::host::MirHost,
    _env: &mut Environment,
) -> Result<Value, String> {
    Err("JIT compiler not available (build with --features jit and LLVM 17)".to_string())
}

/// α.8: 注册 LLVM 内置函数原型（用于 SSA 中的 Call 指令）
///
/// 这些是 JIT 编译时需要提前声明的原型。
#[allow(dead_code)]
fn llvm_type_for_reg_type(_ty: RegType) -> String {
    // TODO: 实现 RegType → LLVM 类型字符串映射
    // RegType::Int → "i64"
    // RegType::Float → "double"
    // RegType::String → "%struct.StringWrapper"
    // RegType::List(RegType::Float) → "[4 x double]"
    "i64".to_string() // 占位
}

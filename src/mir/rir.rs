//! v0.89: RIR/JIR — 9 层架构第 7-8 层（运行时 + JIT 接口）。
//!
//! RIR：运行时自包含（函数表、VTable、效果表、Agent 生命周期）。
//! JIR：增量 JIT（热点分析、guard deopt、热替换）。
//!
//! 本文件定义 trait 接口，不实现具体逻辑。现有 mir/jit.rs 通过
//! LegacyJitBackend 适配层接入。

use crate::mir::core::CoreFunction;
use crate::mir::fcfg::Reg;
use crate::mir::effect::EffectRow;
use crate::mir::lmir::MemLayout;
use crate::typeck::Type;

// ===================================================================
// RIR — 运行时接口
// ===================================================================

/// 编译后的函数（RIR 产出，JIR 消费）。
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub return_type: Type,
    pub effects: EffectRow,
    pub core: CoreFunction,
    pub version: u64,
}

/// 函数版本表 — 热替换基础设施。
#[derive(Debug, Clone, Default)]
pub struct FunctionVersionTable {
    /// 函数名 → 版本列表（最新在末尾）。
    versions: std::collections::HashMap<String, Vec<CompiledFunction>>,
    /// 当前活跃版本索引。
    active: std::collections::HashMap<String, usize>,
}

impl FunctionVersionTable {
    pub fn new() -> Self {
        Self {
            versions: std::collections::HashMap::new(),
            active: std::collections::HashMap::new(),
        }
    }

    /// 注册新版本。
    pub fn register(&mut self, func: CompiledFunction) {
        let name = func.name.clone();
        self.versions.entry(name.clone()).or_default().push(func);
        let count = self.versions[&name].len();
        self.active.insert(name, count - 1);
    }

    /// 获取当前活跃版本。
    pub fn get(&self, name: &str) -> Option<&CompiledFunction> {
        let idx = self.active.get(name)?;
        self.versions.get(name)?.get(*idx)
    }

    /// 热替换：将指定函数切换到最新版本。
    pub fn hot_swap(&mut self, name: &str) -> bool {
        if let Some(versions) = self.versions.get(name) {
            let latest = versions.len() - 1;
            self.active.insert(name.to_string(), latest);
            true
        } else {
            false
        }
    }

    /// 获取函数版本数。
    pub fn version_count(&self, name: &str) -> usize {
        self.versions.get(name).map_or(0, |v| v.len())
    }
}

// ===================================================================
// JIR — JIT 接口
// ===================================================================

/// JIT 编译结果。
#[derive(Debug)]
pub struct JitEntry {
    /// 可执行内存地址。
    pub code_ptr: *const u8,
    /// 代码大小（字节）。
    pub code_size: usize,
    /// 返回值寄存器。
    pub result_reg: Reg,
}

/// JIT 编译错误。
#[derive(Debug, Clone)]
pub enum JitError {
    /// 模板未覆盖（指令类型/平台/操作数类型）。
    CompileReject(String),
    /// 运行时类型 tag 不匹配（bail）。
    GuardFail(String),
    /// 可执行内存分配失败。
    InternalInvariant(String),
}

/// JIT 后端 trait — 消费 LayoutTable（LMIR 产出），不感知 EHIR。
pub trait JitBackend {
    /// 尝试编译一个函数。
    fn try_compile(
        &self,
        func: &CompiledFunction,
        layouts: &LayoutTable,
    ) -> Result<JitEntry, JitError>;

    /// 使指定函数的 JIT 编译失效（下次执行回落解释器）。
    fn invalidate(&mut self, name: &str);

    /// JIT 版本号（每次编译递增）。
    fn version(&self) -> u64;
}

// ===================================================================
// LayoutTable — LMIR 产出，JIR 消费
// ===================================================================

/// 内存布局表 — LMIR 层产出，JIR 层消费。
/// JIR 不感知 Type，只消费 Layout。
#[derive(Debug, Clone, Default)]
pub struct LayoutTable {
    /// 类型名 → 内存布局（来自 lmir::MemLayout）。
    layouts: std::collections::HashMap<String, MemLayout>,
}

impl LayoutTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, type_name: String, layout: MemLayout) {
        self.layouts.insert(type_name, layout);
    }

    pub fn get(&self, type_name: &str) -> Option<&MemLayout> {
        self.layouts.get(type_name)
    }
}

// ===================================================================
// LegacyJitBackend — v0 适配层
// ===================================================================

/// v0 适配层：将现有 mir::jit::JitCompiler 包装为 JitBackend trait。
/// 目前忽略 LayoutTable（现有 JIT 自建类型推断），
/// 但接口已预留 layouts 参数，未来无缝切换。
#[derive(Default)]
pub struct LegacyJitBackend {
    version: u64,
}

impl LegacyJitBackend {
    pub fn new() -> Self {
        Self { version: 0 }
    }
}

impl JitBackend for LegacyJitBackend {
    fn try_compile(
        &self,
        _func: &CompiledFunction,
        _layouts: &LayoutTable,
    ) -> Result<JitEntry, JitError> {
        // v0: 现有 JIT 覆盖率极低（3-5%），暂不实际编译。
        // 返回 CompileReject，调用方回落解释器。
        Err(JitError::CompileReject(
            "LegacyJitBackend: v0 placeholder, use run_mir fallback".to_string(),
        ))
    }

    fn invalidate(&mut self, _name: &str) {
        // v0: no-op
    }

    fn version(&self) -> u64 {
        self.version
    }
}

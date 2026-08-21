//! v0.89: LMIR — 9 层架构第 6 层（设备与内存布局降维）。
//!
//! 将语义值降维为物理表示。LMIR 专注：
//! - Unboxed 原语（Int→i64, Float→f64, Bool→i1）
//! - 内存布局（Alloc/Load/Store/Gep）
//! - GC/引用管理（GcAlloc/GcRoot/GcBarrier/RefCount）
//! - FFI 边界（ExternCall/ExternTypeCast）
//!
//! SIMD 不在本层（已上提到 CMIR 作为数据并行原语，用户建议 #3）。

use crate::mir::fcfg::Reg;

// ===================================================================
// LmirInst — 内存布局感知指令
// ===================================================================

/// LMIR 指令 — 布局感知的低级操作。
#[derive(Debug, Clone)]
pub enum LmirInst {
    // ── Unboxed 常量 ──
    /// 64 位整数（非 tagged union，8 字节）。
    ConstInt(Reg, i64),
    /// 64 位浮点（非 tagged union，8 字节）。
    ConstFloat(Reg, f64),
    /// 布尔值（1 字节）。
    ConstBool(Reg, bool),
    /// 字符串（pointer + length）。
    ConstString(Reg, *const u8, usize),

    // ── 内存操作 ──
    /// 分配内存。
    Alloc {
        dst: Reg,
        layout: MemLayout,
    },
    /// 从内存加载。
    Load {
        dst: Reg,
        src: Reg,
        offset: usize,
        size: usize,
    },
    /// 存储到内存。
    Store {
        dst: Reg,
        offset: usize,
        src: Reg,
        size: usize,
    },
    /// 指针算术（Get Element Pointer）。
    Gep {
        dst: Reg,
        base: Reg,
        offset: Reg,
    },

    // ── GC/引用管理 ──
    /// GC 分配（带追踪）。
    GcAlloc {
        dst: Reg,
        layout: MemLayout,
    },
    /// 注册 GC 根。
    GcRoot(Reg),
    /// GC 写屏障。
    GcBarrier(Reg),
    /// 引用计数操作。
    RefCount {
        dst: Reg,
        src: Reg,
        delta: i32,
    },

    // ── FFI 边界 ──
    /// 调用外部函数。
    ExternCall {
        dst: Reg,
        symbol: String,
        args: Vec<Reg>,
        abi: Abi,
    },
    /// FFI 类型转换。
    ExternTypeCast {
        dst: Reg,
        src: Reg,
        from: LmirType,
        to: LmirType,
    },
}

// ===================================================================
// 辅助类型
// ===================================================================

/// 内存布局描述。
#[derive(Debug, Clone)]
pub struct MemLayout {
    /// 总大小（字节）。
    pub size: usize,
    /// 对齐要求（字节）。
    pub align: usize,
    /// 字段布局：(offset, type)。
    pub fields: Vec<(usize, LmirType)>,
}

/// LMIR 类型 — 物理表示级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LmirType {
    /// 8 字节整数。
    Int64,
    /// 8 字节浮点。
    Float64,
    /// 1 字节布尔。
    Bool,
    /// 指针（8 字节）。
    Ptr,
    /// 原始字节。
    Bytes(usize),
    /// 聚合类型（结构体）。
    Struct(Vec<LmirType>),
    /// 外部类型（FFI）。
    Extern(String),
}

/// FFI 调用约定。
#[derive(Debug, Clone)]
pub enum Abi {
    /// C 调用约定。
    C,
    /// System 调用约定。
    System,
    /// Rust 调用约定。
    Rust,
}

// ===================================================================
// Unbox 策略
// ===================================================================

/// 值的 Unbox 策略 — 由 LMIR 层决定。
#[derive(Debug, Clone)]
pub enum UnboxStrategy {
    /// 直接 unbox（Int→i64, Float→f64, Bool→i1）。
    Direct,
    /// 保持 boxed（String, List, Dict, Closure）。
    Boxed,
    /// 条件 unbox（小值 inline，大值 pointer）。
    SmallInline { threshold_bytes: usize },
}

impl MemLayout {
    /// Int64 布局。
    pub fn int64() -> Self {
        Self { size: 8, align: 8, fields: vec![] }
    }

    /// Float64 布局。
    pub fn float64() -> Self {
        Self { size: 8, align: 8, fields: vec![] }
    }

    /// Bool 布局。
    pub fn bool() -> Self {
        Self { size: 1, align: 1, fields: vec![] }
    }

    /// Pointer 布局。
    pub fn ptr() -> Self {
        Self { size: 8, align: 8, fields: vec![] }
    }
}

impl LmirType {
    /// 获取类型的字节大小。
    pub fn size(&self) -> usize {
        match self {
            LmirType::Int64 => 8,
            LmirType::Float64 => 8,
            LmirType::Bool => 1,
            LmirType::Ptr => 8,
            LmirType::Bytes(n) => *n,
            LmirType::Struct(fields) => fields.iter().map(|f| f.size()).sum(),
            LmirType::Extern(_) => 0, // 外部类型大小未知
        }
    }
}

//! v0.89: Core IR — 9 层架构第 3 层（函数式降维）。
//!
//! CoreInst 是 ~20 种 SSA 可处理的基元指令。高层抽象（闭包、ADT、
//! Thunk、效果处理、TEA）在这一层被降维为基元操作。
//!
//! 设计目标：
//! - SSA 覆盖率 100%（无 passthrough，CoreInst 的所有变体都可优化）
//! - Unboxed 原语（Int→i64, Float→f64, Bool→i1 由 LMIR 处理，
//!   Core 层保持 Value 语义，但指令集已降维为基元）
//! - 效果行已知（EHIR 已推断，Core 消费 EffectLabel）

use crate::common::BinaryOp;
use crate::mir::effect::EffectRow;
use crate::mir::fcfg::Reg;
use crate::typeck::Type;
use crate::value::Value;

// ===================================================================
// CoreInst — 函数式基元指令集
// ===================================================================

/// Core 基元指令 — ~20 种，全部 SSA 可处理。
///
/// 与 MirInst（50 变体，含 35+ passthrough）的关键区别：
/// - 闭包 → ClosureCreate + ClosureCall（非 TaskDef + Call-by-name）
/// - ADT → EnumConstruct + EnumMatch + StructConstruct + StructAccess
/// - 效果 → EffectPerform + EffectInstall + EffectRestore
/// - Thunk → ThunkCreate + ThunkForce
/// - 无声明变体（TraitDef/EnumDef/StructDef 等编译期消失）
/// - 无 I/O 变体（Save/Load/ReadFile 等由 RIR 处理）
#[derive(Debug, Clone)]
pub enum CoreInst {
    // ── 值（8 种）──
    /// 常量加载。
    Const(Reg, Value),
    /// 变量读取（从环境加载）。
    Var(Reg, String),
    /// 寄存器拷贝。
    Copy(Reg, Reg),
    /// 二元运算。
    BinaryOp(Reg, Reg, BinaryOp, Reg),
    /// 闭包调用（非名称调用）。callee 是持有闭包值的寄存器。
    Call(Reg, Reg, Vec<Reg>),
    /// 列表字面量。
    ListLit(Reg, Vec<Reg>),
    /// 字典字面量。
    DictLit(Reg, Vec<(String, Reg)>),
    /// 索引访问。
    Index(Reg, Reg, Reg),

    // ── 环境（3 种）──
    /// 从环境读取变量。
    EnvLoad(Reg, String),
    /// 写入环境（let 绑定）。
    EnvStore(String, Reg),
    /// 原地修改（赋值）。
    EnvMutate(String, Reg),

    // ── 闭包降维 ──
    /// 创建闭包。captures 列出捕获的变量名。
    ClosureCreate {
        dst: Reg,
        params: Vec<String>,
        captures: Vec<String>,
        body: CoreBlock,
    },
    /// 调用闭包。
    ClosureCall(Reg, Reg, Vec<Reg>),

    // ── ADT 降维 ──
    /// 枚举构造。
    EnumConstruct {
        dst: Reg,
        enum_name: String,
        variant: String,
        fields: Vec<Reg>,
    },
    /// 枚举匹配。
    EnumMatch {
        scrutinee: Reg,
        arms: Vec<EnumMatchArm>,
    },
    /// 结构体构造。
    StructConstruct {
        dst: Reg,
        name: String,
        fields: Vec<(String, Reg)>,
    },
    /// 结构体字段访问。
    StructAccess(Reg, Reg, String),

    // ── 效果降维 ──
    /// 执行代数效果。
    EffectPerform {
        dst: Reg,
        label: EffectLabel,
        args: Vec<Reg>,
    },
    /// 安装效果处理器。
    EffectInstall {
        label: EffectLabel,
        handler: Reg,
    },
    /// 恢复效果处理器。
    EffectRestore {
        label: EffectLabel,
    },

    // ── Thunk 降维 ──
    /// 创建惰性求值 thunk。
    ThunkCreate {
        dst: Reg,
        body: CoreBlock,
    },
    /// 强制求值 thunk。
    ThunkForce(Reg, Reg),

    // ── 控制流（5 种）──
    /// 条件分支。
    Branch {
        cond: Reg,
        true_bb: BlockId,
        false_bb: BlockId,
    },
    /// 无条件跳转。
    Jump(BlockId),
    /// 函数返回。
    Return(Option<Reg>),
    /// SSA phi 节点（CMIR 层插入，Core 层保留占位）。
    Phi(Reg, Vec<(BlockId, Reg)>),
    /// 不可达标记。
    Unreachable,

    // ── 副作用（保留，但已降维）──
    /// 表达式语句（丢弃结果）。
    Expr(Reg),
    /// 索引赋值。
    IndexAssign(Reg, Reg, Reg),
}

// ===================================================================
// 辅助类型
// ===================================================================

/// 基本块 ID。
pub type BlockId = usize;

/// Core 基本块。
#[derive(Debug, Clone)]
pub struct CoreBlock {
    pub id: BlockId,
    pub insts: Vec<CoreInst>,
    pub terminator: CoreTerminator,
}

/// Core 终结指令。
#[derive(Debug, Clone)]
pub enum CoreTerminator {
    Branch {
        cond: Reg,
        true_bb: BlockId,
        false_bb: BlockId,
    },
    Jump(BlockId),
    Return(Option<Reg>),
    Unreachable,
}

/// 枚举匹配分支。
#[derive(Debug, Clone)]
pub struct EnumMatchArm {
    pub variant: String,
    pub bindings: Vec<String>,
    pub body: CoreBlock,
}

/// 效果标签（从 EHIR EffectRow 提取）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectLabel {
    Ai,
    Fs,
    Mem,
    Net,
    Bsp,
    Custom(String),
}

// ===================================================================
// Core 函数
// ===================================================================

/// Core 函数 — SSA 形式的基本块集合。
#[derive(Debug, Clone)]
pub struct CoreFunction {
    pub params: Vec<(String, Type)>,
    pub blocks: Vec<CoreBlock>,
    pub entry: BlockId,
    pub effects: EffectRow,
    pub n_regs: usize,
}

// ===================================================================
// EffectLabel 辅助
// ===================================================================

impl EffectLabel {
    /// 从效果名字符串创建 EffectLabel。
    pub fn from_name(name: &str) -> Self {
        match name {
            "Ai" => EffectLabel::Ai,
            "Fs" => EffectLabel::Fs,
            "Mem" => EffectLabel::Mem,
            "Net" => EffectLabel::Net,
            "Bsp" => EffectLabel::Bsp,
            other => EffectLabel::Custom(other.to_string()),
        }
    }

    /// 获取标签名称。
    pub fn name(&self) -> &str {
        match self {
            EffectLabel::Ai => "Ai",
            EffectLabel::Fs => "Fs",
            EffectLabel::Mem => "Mem",
            EffectLabel::Net => "Net",
            EffectLabel::Bsp => "Bsp",
            EffectLabel::Custom(s) => s,
        }
    }
}

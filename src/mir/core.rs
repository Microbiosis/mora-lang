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
//!
//! ## CoreInst vs MirInst 覆盖范围
//!
//! MirInst 有 50 个变体，CoreInst 有 ~20 个。**44/50 MirInst 变体没有
//! CoreInst 直接等价物**。这是设计如此，不是缺口：
//!
//! | 类别 | MirInst 变体 | Core 处理方式 |
//! |------|-------------|--------------|
//! | 声明 | TraitDef/ImplDef/EnumDef/StructDef/TypeAlias | 编译期消失，不进 Core |
//! | TEA | ModelDef/MsgDef/UpdateDef/AppDef | 编译期注册，不进 Core |
//! | I/O | Save/Load/ReadFile/WriteFile/... | RIR 层处理 |
//! | 并发 | Send/Aggregate/Halt/Worker | CMIR 层处理 |
//! | 元编程 | Quasiquote/MacroDef | 编译期展开 |
//! | 效果 | Handle (单节点) | EffectInstall + EffectRestore (拆分) |
//! | 闭包 | TaskDef/Closure | ClosureCreate + ClosureCall |
//! | 控制流 | Label/Jump/JumpIf/JumpIfNot | Branch/Jump (BlockId) |
//! | 管道 | Pipe | Call 链 |
//! | AI | Prompt/PromptSection/DocumentSection | 高层语义，Core 不感知 |
//!
//! Core 的职责是**纯 SSA 可优化子集**，而非 MirInst 的全集映射。
//! 声明/TEA/I/O/并发/元编程等语义由各自层级处理，不经过 Core 优化。

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
        /// 闭包体 —— **一个完整的子函数**（多块 CFG + 入口 + 自己的寄存器空间）。
        ///
        /// v0.104.6 D480：此前是**单个** `CoreBlock` ⇒ 闭包体里的**嵌套分支**
        /// 装不下（子函数降维只保留首块，其余块被丢弃 ⇒ 分支目标指向
        /// **并不存在**的块）。改为承载完整子函数后，闭包体 / match 分支体
        /// 的控制流结构不再丢失。
        body: CoreFunction,
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
    EffectInstall { label: EffectLabel, handler: Reg },
    /// 恢复效果处理器。
    EffectRestore { label: EffectLabel },

    // ── Thunk 降维 ──
    /// 创建惰性求值 thunk。
    ///
    /// v0.104.6 D481：`body` 由**单个** `CoreBlock` 改为**完整子函数** ——
    /// 与 `ClosureCreate.body`（D480）同款截断：thunk 体里的嵌套分支此前
    /// 同样装不下（分支目标指向**并不存在**的块）。
    ThunkCreate { dst: Reg, body: CoreFunction },
    /// 强制求值 thunk。
    ThunkForce(Reg, Reg),

    // ── 区域（结构化作用域）──
    /// 一个**区域构造**：体是**独立的嵌套代码区**（自己的块集合 + 入口 +
    /// 自己的寄存器空间），形状**镜像 MIR 层**的嵌套 `MirFunction`
    /// （`MirInst::Parallel { body: Box<MirFunction> }` 等）。
    ///
    /// v0.104.6 D481：此前这类构造（`parallel` / `observe` / `span` /
    /// `worker` / `transaction` / `with` / `solve` / `section` / `export`）
    /// 在 Core 层是**原地摊平**的 —— 只把体内容降进外层 CFG，构造本身
    /// **完全不可见**。加这个原语后，区域结构在 Core 产物里**看得见了**。
    ///
    /// `kind` 取值：`parallel` / `observe` / `span` / `worker` /
    /// `transaction` / `with_config` / `solve` / `prompt_section` /
    /// `document_section` / `export`。
    Scope {
        kind: String,
        body: CoreFunction,
        /// 补偿体（**仅** `transaction` 有；其余为 `None`）。
        compensation: Option<CoreFunction>,
    },

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
    /// 分支体 —— 与闭包体同款：**一个完整的子函数**（多块 CFG + 入口）。
    ///
    /// v0.104.6 D480：此前是**单个** `CoreBlock` ⇒ 分支体里的嵌套分支被
    /// 截断（目标指向不存在的块）。理由与后果见
    /// [`CoreInst::ClosureCreate`] 的 `body` 字段。
    pub body: CoreFunction,
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
///
/// v0.104.6 D480：这个类型同时承担两种角色 ——
///
/// - **顶层函数**：`ehir_to_core` 的产物（`blocks` 是整个函数的 CFG）；
/// - **嵌套代码区**：闭包体 / match 分支体（`CoreInst::ClosureCreate.body`
///   与 `EnumMatchArm.body`）—— 它们是**独立**的代码区：自己的块集合、
///   自己的入口、**自己的寄存器空间**（与外层不共享编号）。
///
/// 嵌套代码区不是「函数」（没有调用约定、没有形参表 —— 闭包形参在
/// `ClosureCreate.params` 里），但**结构上**是同一件事：一组带入口的块。
/// 复用同一个类型而不是另立一个 `CoreCode`，是为了不引入第二套「代码区」
/// 概念。
#[derive(Debug, Clone)]
pub struct CoreFunction {
    /// 形参（仅顶层有意义；嵌套代码区为空 —— 闭包形参在 `ClosureCreate` 里）。
    pub params: Vec<(String, Type)>,
    pub blocks: Vec<CoreBlock>,
    /// 入口块号。
    pub entry: BlockId,
    /// 效果行（函数级；嵌套代码区继承外层 —— Core 层不按体拆分效果）。
    pub effects: EffectRow,
    /// 寄存器数量（该代码区自己的空间）。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_label_roundtrip() {
        for name in &["Ai", "Fs", "Mem", "Net", "Bsp"] {
            let label = EffectLabel::from_name(name);
            assert_eq!(label.name(), *name);
        }
        let custom = EffectLabel::from_name("MyEffect");
        assert_eq!(custom.name(), "MyEffect");
        assert!(matches!(custom, EffectLabel::Custom(_)));
    }

    #[test]
    fn core_function_default() {
        let func = CoreFunction {
            params: vec![("x".to_string(), Type::Int)],
            blocks: vec![],
            entry: 0,
            effects: EffectRow::Empty,
            n_regs: 1,
        };
        assert_eq!(func.params.len(), 1);
        assert_eq!(func.n_regs, 1);
    }

    #[test]
    fn core_block_terminator() {
        let block = CoreBlock {
            id: 0,
            insts: vec![CoreInst::Const(0, Value::Int(42))],
            terminator: CoreTerminator::Return(Some(0)),
        };
        assert_eq!(block.id, 0);
        assert_eq!(block.insts.len(), 1);
    }
}

//! mora Intermediate Representation (MIR) — α.0
//!
//! 寄存器式线性指令序列。AST → MIR lowering 产物，也是 MIR 解释器执行格式。
//! SSA 构造 pass（MIR-plain → MIR-ssa）在 α.3 加入，此处先只有 MIR-plain。
//!
//! α.0 覆盖范围：Const / Var / BinaryOp / Call / Define / Assign /
//! IndexAssign / Expr / Label / Jump / JumpIf / JumpIfNot / Return / Break /
//! Continue / ListLit / DictLit / Index / MethodCall / Pipe / Prompt /
//! MatchArm / TaskDef / ToolDef / Import / WithConfig / StreamFor
//!
//! 对应 AST：
//! - ExprKind: Literal/Variable/Binary/Pipe/Call/MethodCall/Index/Closure/Match/
//!   Prompt/RouteCall/AiModelCall/Question/NamespaceRef/DynTrait/Grouping/List/
//!   Dict/Borrow/BorrowMut/Command/Send (22 variants)
//! - StmtKind: Let/Assign/IndexAssign/TaskDef/If/For/Return/Import/Parallel/Match/
//!   Save/Load/ReadFile/WriteFile/AppendFile/ReadBytesFile/WriteBytesFile/Expr/
//!   With/StreamFor/ToolDef/Break/Continue/Route/Observe/Span/RecordTokens/TraitDef/
//!   ImplDef/Worker/Send/Receive/Transaction/Commit/Rollback/MacroDef/TypeAlias/
//!   Export/ReExport (37 variants)

use crate::common::BinaryOp;
use crate::value::Value;

pub mod interp;
pub mod jit;
pub mod lower;
pub mod opt;
pub mod ssa;
pub mod typeinfer;

pub use interp::run_mir;
pub use lower::lower_program;

/// 虚拟寄存器索引（无限数量，lowering 时计数器分配）
pub type Reg = usize;

/// 跳转目标（body 中的指令索引）
pub type Label = usize;

/// 一个 MIR 函数 = 一段脚本或一个 task body
#[derive(Debug, Clone)]
pub struct MirFunction {
    pub params: Vec<String>,
    pub body: Vec<MirInst>,
    pub n_regs: usize,
}

/// MIR 指令（α.0 + α.1 子集）
#[derive(Debug, Clone)]
pub enum MirInst {
    // ── 值指令（产生结果到 dst 寄存器）──
    Const(Reg, Value),
    Var(Reg, String),
    BinaryOp(Reg, Reg, BinaryOp, Reg),
    /// 函数调用。callee 是名字（ExprKind::Call 的 callee 是 String），非寄存器
    Call(Reg, String, Vec<Reg>),
    /// α.1: 列表字面量 [r0, r1, ...]
    ListLit(Reg, Vec<Reg>),
    /// α.1: 字典字面量 {key: val, ...}（key 是 String，val 是 Reg）
    DictLit(Reg, Vec<(String, Reg)>),
    /// α.1: 索引 obj[idx] → dst
    Index(Reg, Reg, Reg),
    /// α.1: 索引赋值 obj[idx] = val（返回赋值结果）
    IndexAssign(Reg, Reg, Reg),
    /// α.1: 方法调用 recv.method(args) → dst
    MethodCall(Reg, Reg, String, Vec<Reg>),
    /// α.1: 管道 lhs |> callee → dst（callee 是 reg 里的可调用值）
    Pipe(Reg, Reg, Reg),
    /// α.1: p"..." 模板拼接（不触发 AI，只拼接 parts 的字符串形式）
    Prompt(Reg, Vec<Reg>),
    /// α.0: 模式匹配表达式。arms 依次尝试，命中第一个即返回 arm_val。
    /// arms: (pattern_str, condition_reg_or_None, body_mir_func, output_reg)
    MatchExpr {
        val: Reg,
        arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)>,
    },

    // ── 语句指令（副作用）──
    Define(String, Reg),
    Assign(String, Reg),
    Expr(Reg),

    /// 模式匹配分支：cond_reg 非空时表示条件守卫，空时表示默认分支
    /// 由 Match lowering 生成多个 MatchArm，解释器依次匹配
    MatchArm {
        cond_reg: Option<Reg>,
        body: Box<MirFunction>,
    },

    /// α.2: task 定义。body 是嵌套 MirFunction，解释器递归执行。
    TaskDef {
        name: String,
        params: Vec<String>,
        body: Box<MirFunction>,
    },

    /// α.2: tool 定义。body 是嵌套 MirFunction；params/return_type 用于 schema。
    /// 解释器注册为 Value::Tool 到 environment + ToolDef 到 tool_registry。
    ToolDef {
        name: String,
        description: String,
        params: Vec<String>,
        return_type: Option<String>,
        body: Box<MirFunction>,
        exported: bool,
    },

    /// α.2: import 语句。解释器读文件+解析+执行（委托 AST 路径）。
    Import(String),

    /// α.2: with 块。bindings 设置 AI config，body 执行后恢复。
    /// 解释器保存/恢复 current_ai_config。
    /// jit=true 时，block 内容通过 SSA → LLVM → JIT 编译执行。
    WithConfig {
        bindings: Vec<(String, Reg)>,
        body: Box<MirFunction>,
        jit: bool,
    },

    /// α.2: 流式循环。stream_for var in prompt body end
    /// prompt_reg 为字符串表达式，body 是嵌套 MirFunction。
    /// 解释器流式执行（逐 token 触发 AI）。
    StreamFor {
        prompt_reg: Reg,
        var: String,
        body: Box<MirFunction>,
    },

    // ── 控制流（替代 FlowSignal 枚举传返）──
    Label(Label),
    Jump(Label),
    JumpIf(Reg, Label),
    JumpIfNot(Reg, Label),
    Return(Option<Reg>),
    /// α.1: break 到指定 label（循环出口）
    Break(Label),
    /// α.1: continue 到指定 label（循环增量处）
    Continue(Label),
}

impl MirFunction {
    // Label 在 body 中的实际索引。lowering 时 Label 占位，finish 时回填。
    // α.0 简化：Label 指令本身就是目标，Jump 的 label 是 body 索引。
    pub fn label_index(&self, label: Label) -> usize {
        label
    }
}

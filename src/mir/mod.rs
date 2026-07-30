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

pub mod expr;
pub mod interp;
pub mod jit;
pub mod lower;
pub mod opt;
pub mod optimize;
pub mod ssa;
pub mod typeinfer;

pub use interp::run_mir;
pub use expr::MirExpr;
// lower_program removed in Phase A (v0.55) — use lower_mir_exprs instead

/// 虚拟寄存器索引（无限数量，lowering 时计数器分配）
pub type Reg = usize;

/// 跳转目标（body 中的指令索引）
pub type Label = usize;

/// 一个 MIR 函数 = 一段脚本或一个 task body
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction {
    pub params: Vec<String>,
    pub body: Vec<MirInst>,
    pub n_regs: usize,
}

/// MIR 指令（α.0 + α.1 子集）
#[derive(Debug, Clone, PartialEq)]
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

    /// α.10: 闭包字面量。body 是嵌套 MirFunction（独立寄存器空间），
    /// 解释器构造 Value::Closure { mir_body: Arc<MirFunction> }。
    /// 调用时 dispatch 直接走 run_mir。
    Closure {
        dst: Reg,
        params: Vec<String>,
        body: Box<MirFunction>,
    },
    /// α.12: dyn Trait 包装。解释器构造 Value::TraitObject { data, trait_name }。
    /// vtable 派发由  处理（call_method 命中 TraitObject 分支）。
    DynTrait {
        dst: Reg,
        src: Reg,
        trait_generics: Vec<String>,
        trait_name: String,
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

    // ── 类型定义语句（α.3: 与 AST execute 语义一致）──
    /// α.3: 类型别名。定义 `name` → `target` 的字符串映射。
    TypeAlias {
        name: String,
        target: String,
    },

    /// α.3: 枚举定义。定义 `name` → Dict(variant_name → String)。
    EnumDef {
        name: String,
        variants: Vec<crate::common::EnumVariant>,
    },

    /// α.3: 结构体定义。定义 `name` → Closure(构造器)。
    StructDef {
        name: String,
        fields: Vec<crate::common::StructField>,
    },

    // ── 宏定义（α.5: 与 AST execute_macro_def 语义一致）──
    /// α.5: macro def — 注册 Value::Macro(name, params) 到环境。
    MacroDef {
        name: String,
        params: Vec<String>,
    },

    // ── 运行时特性（α.4: transaction / worker）──
    /// α.4: 事务。body 执行成功则正常返回；失败则执行 compensation 后返回错误。
    Transaction {
        body: Box<MirFunction>,
        compensation: Box<MirFunction>,
    },

    /// α.4: send — 发送值到 worker channel（target 是 channel 名称）。
    Send {
        value: Reg,
        target: String,
    },

    /// α.4: receive — 从 worker channel 接收值并绑定到 var。
    Receive {
        var: String,
        source: String,
    },

    /// α.4: rollback — 触发事务回滚（返回 "Transaction rolled back" 错误）。
    Rollback,

    /// α.5: worker — 并发 worker。body 顺序执行（与 AST 语义一致）。
    Worker {
        name: String,
        body: Box<MirFunction>,
    },

    /// α.5: commit — 事务提交（no-op，与 AST 语义一致）。
    Commit,

    /// α.5: route — 路由声明（不实现，返回错误）。
    Route(String),

    /// α.5: observe — 可观测性块。执行 body，配置信息记录但无副作用。
    Observe {
        config: String,
        body: Box<MirFunction>,
    },

    /// α.5: span — 追踪 span。执行 body，name 记录但不执行实际追踪。
    Span {
        name: String,
        body: Box<MirFunction>,
    },

    /// α.5: record_tokens — 记录 token 输入输出（no-op）。
    RecordTokens {
        input: String,
        output: String,
    },

    // ── 文件 I/O（α.6: Save/Load/ReadFile/WriteFile/AppendFile/ReadBytesFile/WriteBytesFile）──
    /// α.6: save — 将 value 序列化为文件。
    Save {
        path: Reg,
        value: Reg,
    },

    /// α.6: load — 从文件加载 JSON 值并绑定到 var。
    Load {
        path: Reg,
        var: String,
    },

    /// α.6: read_file — 读取文件为字符串，绑定到 var。
    ReadFile {
        path: Reg,
        var: String,
    },

    /// α.6: write_file — 将 content 写入文件。
    WriteFile {
        path: Reg,
        content: Reg,
    },

    /// α.6: append_file — 将 content 追加到文件。
    AppendFile {
        path: Reg,
        content: Reg,
    },

    /// α.6: read_bytes_file — 读取文件为字节数组，绑定到 var。
    ReadBytesFile {
        path: Reg,
        var: String,
    },

    /// α.6: write_bytes_file — 将 hex 字节写入文件。
    WriteBytesFile {
        path: Reg,
        content: Reg,
    },

    // ── 类型系统（α.7: TraitDef/ImplDef）──
    /// v0.55: trait def — 完全 MIR-native，methods 是 MirTraitMethod 而非 ast_v2::TraitMethod。
    TraitDef {
        name: String,
        parents: Vec<String>,
        methods: Vec<crate::mir::expr::MirTraitMethod>,
        /// prelowered method bodies (parallel to methods)，让默认实现走 run_mir。
        method_bodies: Vec<MirFunction>,
    },

    /// v0.55: impl def — 完全 MIR-native。
    ImplDef {
        trait_name: String,
        trait_generics: Vec<String>,
        for_type: String,
        for_generics: Vec<String>,
        methods: Vec<crate::mir::expr::MirFnDef>,
        /// prelowered method bodies (parallel to methods)。
        method_bodies: Vec<MirFunction>,
    },

    // ── 高级特性（α.8: orchestrate/skill/prompt/document/eval）──
    /// v0.55: orchestrate — 编排执行。
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: Box<crate::mir::expr::MirOrchestrateKind>,
    },

    /// α.8: eval — 断言测试。
    Eval {
        name: String,
        given_reg: Reg,
        expects: Vec<Reg>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },

    /// v0.55: skill def — 完全 MIR-native。
    SkillDef {
        name: String,
        description: Option<String>,
        version: Option<String>,
        requires: Vec<String>,
        tasks: Vec<crate::mir::expr::MirSkillTask>,
        /// prelowered task bodies (parallel to tasks)。
        task_bodies: Vec<MirFunction>,
        verify: Option<crate::mir::expr::MirSkillVerify>,
        /// α.11: prelowered verify body。
        verify_body: Option<MirFunction>,
    },

    /// α.8: prompt section — 扫描 body，构建 Value::PromptSection 到环境。
    PromptSection {
        name: String,
        body: Box<MirFunction>,
    },

    /// α.8: document section — 扫描 body，构建 Value::DocumentSection 到环境。
    DocumentSection {
        name: String,
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

//! v0.89: FCFG (Frontend Control Flow Graph) — 9 层 IR 架构第 1 层。
//!
//! `Node<M>` 是 IR 管线的泛型基础节点。通过元数据参数 `M` 区分阶段：
//! - `Node<()>` = FCFG（无类型，parser 直接输出）
//! - `Node<TypeInfo>` = EHIR（带类型标签，typeck 后）
//!
//! 设计原则（来自用户 5 点验证建议 #1）：
//! - FCFG 和 EHIR 物理合并为 `Node<M>` 泛型，逻辑层保持分离
//! - 遍历逻辑只写一次，`Node<()>` 时忽略 meta，`Node<TypeInfo>` 时消费 meta
//! - 结构化控制流（If/While/For/Match），非 Jump/Label
//!
//! 与现有 IR 的关系：
//! - WitnessKind (30 变体) → Node`<M>` 的子集映射（树形 → 结构化 CFG；typeck 消费面）
//! - MirInst (50 变体) → Node`<M>` 不直接映射（MirInst 是线性的，Node 是结构化的）

use crate::common::{BinaryOp, Literal, Span};
use crate::mir::effect::EffectRow;
use crate::typeck::Type;

// ===================================================================
// Node<M> — 泛型 IR 节点
// ===================================================================

/// 泛型 IR 节点 — 通过 `M` 参数区分阶段。
///
/// - `M = ()`：FCFG（无类型信息）
/// - `M = TypeInfo`：EHIR（带类型标签）
///
/// 每个节点有一个 `meta: M` 字段，遍历时可选择性消费或忽略。
/// `span` 是源码位置，所有层共享（用于错误报告和影子表映射）。
#[derive(Debug, Clone)]
pub enum Node<M> {
    // ── 值产生 ──
    Literal {
        reg: Reg,
        value: Literal,
        span: Span,
        meta: M,
    },
    Variable {
        reg: Reg,
        name: String,
        span: Span,
        meta: M,
    },
    BinaryOp {
        dst: Reg,
        lhs: Reg,
        op: BinaryOp,
        rhs: Reg,
        span: Span,
        meta: M,
    },
    Call {
        dst: Reg,
        callee: Reg,
        /// 函数名（已知时填充，桥接层直接使用）。
        callee_name: Option<String>,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    MethodCall {
        dst: Reg,
        receiver: Reg,
        method: String,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    /// 逻辑或。WitnessKind::Or 对应。
    ///
    /// v0.104.6 D477：**rhs 是「块」而不是已算好的寄存器**。
    ///
    /// 短路求值要求 rhs **只在 lhs 为假时**才求值 ⇒ rhs 的求值必须能被放进
    /// 「非短路分支」里。承载寄存器（`rhs: Reg`）的形状**结构上无法**表达
    /// 短路求值：寄存器在节点降级之前就已写好，rhs 的副作用早已发生
    /// （D477 前的生产路径正是如此 —— 见 `fcfg_lower` 的降级注释）。
    ///
    /// 降级后的指令序列（`fcfg_lower`）：
    ///
    /// ```text
    ///   JumpIf(l, short)          ; l 为真 ⇒ 走短路（rhs 不求值）
    ///   <rhs 块的指令>            ; ← 只在这条路上求值
    ///   BinaryOp(dst, l, .., r)   ; fall-through：l 为假 ⇒ 结果 = r
    ///   Jump(end)
    /// short:
    ///   Copy(dst, l)              ; 短路：结果 = l
    /// end:
    /// ```
    Or {
        dst: Reg,
        lhs: Reg,
        /// rhs 的**表达式块**（未求值）：只在 lhs 为假时求值。
        rhs: Block<M>,
        span: Span,
        meta: M,
    },
    /// 逻辑与。WitnessKind::And 对应。
    ///
    /// v0.104.6 D477：rhs 是**未求值的块**（契约同 [`Node::Or`]）——
    /// 只在 lhs 为真时求值。
    And {
        dst: Reg,
        lhs: Reg,
        /// rhs 的**表达式块**（未求值）：只在 lhs 为真时求值。
        rhs: Block<M>,
        span: Span,
        meta: M,
    },
    /// dyn Trait 强制转换。WitnessKind::DynTrait 对应。
    DynTrait {
        dst: Reg,
        src: Reg,
        trait_name: String,
        span: Span,
        meta: M,
    },
    /// AI prompt 模板 p"..."。WitnessKind::Prompt 对应。
    Prompt {
        dst: Reg,
        parts: Vec<Reg>,
        span: Span,
        meta: M,
    },
    /// 匿名闭包表达式 fn(x) ... end。WitnessKind::Closure 对应。
    ClosureExpr {
        dst: Reg,
        params: Vec<Param>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    ListLit {
        dst: Reg,
        items: Vec<Reg>,
        span: Span,
        meta: M,
    },
    DictLit {
        dst: Reg,
        entries: Vec<(String, Reg)>,
        span: Span,
        meta: M,
    },
    Index {
        dst: Reg,
        obj: Reg,
        idx: Reg,
        span: Span,
        meta: M,
    },

    // ── 控制流（结构化，非 Jump/Label）──
    If {
        cond: Reg,
        then: Block<M>,
        else_: Option<Block<M>>,
        /// v0.104.2: `if` 作为**表达式**的结果寄存器（两分支各 Copy 一次到
        /// 它）。与 `While::dst` 同一理由：缺此字段时 `node_result_reg_of`
        /// 只能返回哨兵 0，`let x = if c then "a" else "b" end` 会把 `x`
        /// 绑到寄存器 0（恰好是前面某个绑定的值）—— spec §7.1 的
        /// `let x = if cond then "a" else "b" end` 因此静默取到错值。
        dst: Reg,
        span: Span,
        meta: M,
    },
    While {
        cond: Block<M>,
        body: Block<M>,
        /// 循环作为表达式的结果寄存器（Nil）—— 与 emit.rs 的循环 emit
        /// 契约一致（循环在指令流末尾留一个 Const(Nil) 供值位置消费者
        /// 读取）。缺此字段时下层只能回退到寄存器 0。
        dst: Reg,
        span: Span,
        meta: M,
    },
    For {
        var: String,
        iter: Reg,
        body: Block<M>,
        /// 循环作为表达式的结果寄存器（Nil）—— 同 `While::dst`。
        dst: Reg,
        span: Span,
        meta: M,
    },
    Match {
        /// match 表达式的统一结果寄存器 — 所有 arm 的 output_reg
        /// 共用此寄存器（h_match_expr 写入，消费者读取）。
        dst: Reg,
        scrutinee: Reg,
        arms: Vec<MatchArm<M>>,
        span: Span,
        meta: M,
    },
    Return {
        value: Option<Reg>,
        span: Span,
        meta: M,
    },
    Break {
        /// v0.90.4: 跳转 label（witness_to_fcfg 用 loop_stack 填）。
        /// fcfg_lower 不再依赖独立 loop_stack（label 由节点携带）。
        label: usize,
        span: Span,
        meta: M,
    },
    Continue {
        label: usize,
        span: Span,
        meta: M,
    },

    // ── 绑定 ──
    Let {
        name: String,
        type_ann: Option<TypeAnnotation>,
        value: Reg,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Assign {
        name: String,
        value: Reg,
        span: Span,
        meta: M,
    },
    IndexAssign {
        obj: Reg,
        idx: Reg,
        value: Reg,
        span: Span,
        meta: M,
    },

    // ── 声明（编译期注册，不进运行时）──
    FnDef {
        name: String,
        params: Vec<Param>,
        return_ann: Option<TypeAnnotation>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    TypeAlias {
        name: String,
        target: TypeAnnotation,
        span: Span,
        meta: M,
    },
    EnumDef {
        name: String,
        variants: Vec<Variant>,
        span: Span,
        meta: M,
    },
    StructDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        span: Span,
        meta: M,
    },
    TraitDef {
        name: String,
        methods: Vec<TraitMethod>,
        span: Span,
        meta: M,
    },
    ImplDef {
        trait_name: String,
        for_type: TypeAnnotation,
        methods: Vec<(String, Block<M>)>,
        span: Span,
        meta: M,
    },
    Import {
        path: String,
        span: Span,
        meta: M,
    },
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Block<M>,
        span: Span,
        meta: M,
    },

    // ── 代数效果 ──
    Perform {
        /// perform 表达式的结果寄存器（handler 决定具体值）。
        dst: Reg,
        effect: String,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    Handle {
        /// v0.104.6 D35：handle 整体的结果寄存器（`h_handle` 写入 body 末尾
        /// 表达式的值）。**本字段此前不存在**，而 `Perform` / `If` / `Match`
        /// 等所有「表达式节点」都有 —— 缺它导致：
        ///
        /// * `node_result_reg_of` 匹配不到 `Handle` → 落 `_ => None` →
        ///   兜底返回**哨兵 0**，于是 `let r = handle …` 的 `Let` 节点引用
        ///   寄存器 0；
        /// * `fcfg_lower` 只能**新分配** `k_dst = ctx.alloc_reg()`，而
        ///   `lower_fcfg(nodes: &[Fcfg])` 收的是不可变切片，**写不回去**通知
        ///   那个 `Let`。
        ///
        /// 结果两端脱节：`Define("r", 0)` 引用一个无任何生产者写过的寄存器 →
        /// DAG 执行器 `node_ready` 恒 false → `Define` 永不激活 → 它所在的
        /// Sequence 链断裂 → **其后所有 Effect 节点（含 `print`）静默消失**，
        /// 无报错、退出码 0。判决实验：`MORA_9LAYER=0`（走 `emit.rs` 单遍
        /// 直出、不经本 IR）现象消失。
        /// 〔历史记档：`MORA_9LAYER=0` 已于 v0.104.6 D474 退役〕
        dst: Reg,
        effect: String,
        body: Block<M>,
        handler: Block<M>,
        k_param: String,
        span: Span,
        meta: M,
    },

    // ── TEA ──
    ModelDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        /// v0.104.6 D412：字段默认值的**表达式节点**，供 `fcfg_lower` 发射
        /// `DictLit` + `Define("Name.defaults")` —— 与 `emit_definitions.rs`
        /// 的 emit 流逐条对齐。没有它，9 层管线会漏掉默认值导致差分失败回落。
        ///
        /// 元组是 `(字段名, 结果寄存器, 表达式节点)` —— 寄存器在
        /// `witness_to_fcfg` 里由 `node_result_reg` **一次算好**带进来，
        /// 这样 `fcfg_lower` 与 `max_reg_in_node` 都不必重复那个 match。
        defaults: Vec<(String, Reg, Node<M>)>,
        span: Span,
        meta: M,
    },
    MsgDef {
        name: String,
        variants: Vec<Variant>,
        span: Span,
        meta: M,
    },
    UpdateDef {
        name: String,
        params: Vec<Param>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    AppDef {
        name: String,
        model: String,
        msg: String,
        init: Block<M>,
        /// v0.104: update/view 的**形参名**（来自 `WitnessKind::Closure`）。
        /// 运行期 `h_app_def` 按位置绑定 `MirFunction.params` —— 缺此字段时
        /// 只能硬编码 `["model","msg"]`，用户声明别的形参名就取不到值。
        update_params: Vec<Param>,
        update: Block<M>,
        view_params: Vec<Param>,
        view: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.102: 声明式范式（逻辑式/关系式）──
    /// 关系定义 —— 编译期子句模板数据随节点携带（lower 直接取用）。
    RelDef {
        name: String,
        clauses: Vec<crate::rel::Clause>,
        span: Span,
        meta: M,
    },
    /// solve 查询 —— goal 是目标构建体块。
    ///
    /// v0.104.6 D120：`dst` 是**解列表**的结果寄存器。缺此字段时
    /// `node_result_reg_of` 落到 `_ => None` → `unwrap_or(0)` 哨兵 →
    /// `let r = solve { … }` 的就绪门槛恒 false → 其后整条 Sequence 链
    /// **静默饿死**（无报错、退出码 0）。与 D35(`Handle`)/D58(`WithConfig`)
    /// 同一失败模式，那两个已修，此处漏了。
    ///
    /// 由 `witness_to_fcfg` 用**该层**的 `b.alloc()` 预分配 —— `fcfg_lower`
    /// 必须复用它，不得另 alloc，否则两条寄存器序列对不上。
    Solve {
        dst: Reg,
        limit: Option<usize>,
        query_vars: Vec<String>,
        anon_vars: Vec<String>,
        goal: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.103: 命名 section 声明 ──
    PromptSection {
        name: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    DocumentSection {
        name: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.103: 可观测性块 ──
    Observe {
        config: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Span {
        name: String,
        tags: Vec<(String, String)>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Parallel {
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Export {
        names: Vec<String>,
        decl: Block<M>,
        span: Span,
        meta: M,
    },

    // ── 元编程 ──
    Quasiquote {
        dst: Reg,
        segments: Vec<QuasiquoteSegment>,
        span: Span,
        meta: M,
    },

    // ── 编排 ──
    Orchestrate {
        input_var: String,
        result_var: String,
        /// v0.104.6 D473：编排载荷**原样**承载 witness 的
        /// `WitnessOrchestrateKind`（不再经 `fcfg::OrchestrateKind<M>` 投影）。
        ///
        /// 旧投影是**有损**的：agent 体、边条件、state_schema / checkpoint /
        /// interrupt_points / adjacency、MoA/MoE 的 prompt 全部丢失 ——
        /// 而它的唯一消费者（`fcfg_lower`）本来就把一切替换成
        /// `Sequential { agents: vec![] }` 占位。
        ///
        /// ⇒ 编排是**数据构造语句**（不参与控制流），其载荷是**数据**而非
        /// CFG 结构，故按原样透传（与 `Node::RelDef { clauses }` 同款）；
        /// witness→Mir 的转换由 FCFG→MIR 桥接层（`fcfg_lower`）负责 ——
        /// 那正是该层的职责。
        kind: crate::mir::witness::WitnessOrchestrateKind,
        /// 语句自身的结果寄存器（值为 Nil）—— 与 emit 路径同形。
        dst: Reg,
        span: Span,
        meta: M,
    },

    // ── 配置块 ──
    WithConfig {
        bindings: Vec<(String, Reg)>,
        body: Block<M>,
        /// v0.104.6 D58：块自身的**结果寄存器**。
        ///
        /// `with` 块的语义是「配一段上下文，跑块体，值不向外传播」
        /// （emit 路径 `emit_with_w` 的注释即写着「子 body 的返回值不传播」，
        /// 随后补一条 `Const(dst, Nil)`）。此前本节点**没有**结果寄存器，
        /// 于是 `witness_to_fcfg::node_result_reg_of` 落 `_ => None`
        /// → `unwrap_or(0)` → 块结果被指向**寄存器 0**，而 reg 0 恰恰是
        /// 第一个配置绑定值 —— `with model = "gpt-4o" / 2 / end` 的块结果
        /// 因此变成 `String("gpt-4o")`（裸路径是 `Nil`）。
        ///
        /// 与 D35 给 `Node::Handle` 补 `dst` 是同一类修复、同一种失败形态。
        dst: Reg,
        span: Span,
        meta: M,
    },

    // ── v0.104.6 D472：9 层管线补齐 ──
    //
    // 这 4 类构造此前在 FCFG 层**没有对应节点**，所以 9 层降级链
    // 结构上产不出 `Worker` / `Transaction` / `Eval` / `Aggregate` 指令 ——
    // 不是「降级链没写」，是**连节点类型都没有**。
    // 现与 WitnessKind 的 4 个新变体、MirInst 的 4 个变体一一对应。
    /// worker 块 —— 与 `MirInst::Worker { name, body }` 同构。
    Worker {
        name: String,
        body: Block<M>,
        /// 块自身的结果寄存器（值为 Nil）—— 语句型构造必须有值。
        dst: Reg,
        span: Span,
        meta: M,
    },
    /// transaction 块 —— 与 `MirInst::Transaction { body, compensation }` 同构。
    Transaction {
        body: Block<M>,
        compensation: Block<M>,
        /// 块自身的结果寄存器（值为 Nil）。
        dst: Reg,
        span: Span,
        meta: M,
    },
    /// eval 断言 —— 与 `MirInst::Eval { name, given_reg, expects, ... }` 对应。
    Eval {
        name: String,
        given: Reg,
        expects: Vec<Reg>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
        /// 语句自身的结果寄存器（值为 Nil）。
        dst: Reg,
        span: Span,
        meta: M,
    },
    /// aggregate 贡献 —— 与 `MirInst::Aggregate { name, value }` 对应。
    Aggregate {
        name: String,
        value: Reg,
        /// 语句自身的结果寄存器（值为 Nil）。
        dst: Reg,
        span: Span,
        meta: M,
    },

    // ── v0.104.6 D472：管道与事务家族补齐 ──
    /// 管道 `x |> f` —— 与 `MirInst::Pipe { dst, lhs, rhs }` 同构。
    Pipe {
        dst: Reg,
        left: Reg,
        right: Reg,
        span: Span,
        meta: M,
    },
    /// `commit` —— 与 `MirInst::Commit` 同构（unit 指令）。
    Commit {
        /// 语句自身的结果寄存器（值为 Nil）。
        dst: Reg,
        span: Span,
        meta: M,
    },
    /// `rollback` —— 与 `MirInst::Rollback` 同构（unit 指令）。
    Rollback {
        /// 语句自身的结果寄存器（值为 Nil）。
        dst: Reg,
        span: Span,
        meta: M,
    },

    // ── 序列 ──
    Sequence {
        nodes: Vec<Node<M>>,
        span: Span,
        meta: M,
    },
    // ── 表达式语句（丢弃结果）──
    Expr {
        reg: Reg,
        span: Span,
        meta: M,
    },
}

// ===================================================================
// 辅助类型
// ===================================================================

/// 寄存器标识符。
pub type Reg = usize;

/// 基本块 — 一组顺序执行的节点。
#[derive(Debug, Clone)]
pub struct Block<M> {
    pub nodes: Vec<Node<M>>,
    /// 块的最后一个寄存器（表达式块的值）。
    pub result: Option<Reg>,
}

/// 模式匹配分支。
#[derive(Debug, Clone)]
pub struct MatchArm<M> {
    pub pattern: Pattern,
    /// v0.104.3: 守卫是**块体**（独立寄存器空间、延迟求值），不再是外层寄存器。
    ///
    /// 守卫引用**模式绑定变量**（`x when x > 0`），而那些绑定只在
    /// `h_match_expr` 匹配成功时才进入 env —— 外层求值读不到，守卫恒假。
    /// 与 `body` 同构：lowering 后成为独立 `MirFunction`，由 handler 在绑定
    /// 之后调用。
    pub guard: Option<Block<M>>,
    pub body: Block<M>,
}

/// 函数参数。
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub type_ann: Option<TypeAnnotation>,
    pub default: Option<Literal>,
}

/// 类型注解（源码级，未解析）。
#[derive(Debug, Clone)]
pub struct TypeAnnotation(pub String);

/// 枚举/消息变体。
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub payload: Option<TypeAnnotation>,
}

/// Trait 方法声明。
#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_ann: Option<TypeAnnotation>,
}

/// 模式。
#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Variable(String),
    Literal(Literal),
    Tuple(Vec<Pattern>),
    List(Vec<Pattern>),
    ListVec {
        head: Vec<Pattern>,
        tail: Option<Box<Pattern>>,
    },
    Dict(Vec<(String, Pattern)>),
    TypeAscription(Box<Pattern>, TypeAnnotation),
}

/// Quasiquote 段。
#[derive(Debug, Clone)]
pub enum QuasiquoteSegment {
    Quote(String),
    Unquote(Reg),
    UnquoteSplice(Reg),
}

// ── v0.104.6 D473：删除 `fcfg::OrchestrateKind<M>` / `PregelConfig<M>` /
// `MoALayer<M>` ──
//
// 它们是 witness 编排载荷的**有损投影**：agent 体、边条件、state_schema /
// checkpoint / interrupt_points / adjacency、MoA/MoE 的 prompt 全部丢失
// （MoE 甚至把专家名写成了 `format!("{:?}", span)`）。
//
// 而它们的**唯一消费者**（`fcfg_lower`）本来就把投影结果整体替换成
// `Sequential { agents: vec![] }` 占位 —— 即：这份有损数据**从头到尾没被
// 用过**，只让 9 层管线产不出真正的 `MirInst::Orchestrate`（于是 orchestrate
// 程序必然差分失败、必然回落旧路径）。
//
// ⇒ 现由 `Node::Orchestrate::kind` 直接承载 witness 载荷
// （`WitnessOrchestrateKind`），转换在 FCFG→MIR 桥接层完成。

// ===================================================================
// EHIR 元数据
// ===================================================================

/// EHIR 阶段的元数据 — 每个节点携带类型信息。
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// 推断出的类型。
    pub ty: Type,
    /// 该节点的效果行。
    pub effects: EffectRow,
    /// 源码位置。
    pub span: Span,
}

impl TypeInfo {
    /// 创建带类型的 TypeInfo。
    pub fn new(ty: Type, effects: EffectRow, span: Span) -> Self {
        Self { ty, effects, span }
    }

    /// 创建未知类型的 TypeInfo（TypeAny + Empty effect）。
    pub fn unknown(span: Span) -> Self {
        Self {
            ty: Type::Any,
            effects: EffectRow::Empty,
            span,
        }
    }
}

// ===================================================================
// 类型别名
// ===================================================================

/// FCFG — 无类型的前端控制流图。
pub type Fcfg = Node<()>;

/// EHIR — 带类型的早期高 IR。
pub type Ehir = Node<TypeInfo>;

/// FCFG 块。
pub type FcfgBlock = Block<()>;

/// EHIR 块。
pub type EhirBlock = Block<TypeInfo>;

// ===================================================================
// Node<M> 辅助方法
// ===================================================================

impl<M> Node<M> {
    /// 获取节点的源码位置。
    pub fn span(&self) -> Span {
        match self {
            Node::Literal { span, .. }
            | Node::Variable { span, .. }
            | Node::BinaryOp { span, .. }
            | Node::Call { span, .. }
            | Node::MethodCall { span, .. }
            | Node::Or { span, .. }
            | Node::And { span, .. }
            | Node::DynTrait { span, .. }
            | Node::Prompt { span, .. }
            | Node::ClosureExpr { span, .. }
            | Node::ListLit { span, .. }
            | Node::DictLit { span, .. }
            | Node::Index { span, .. }
            | Node::If { span, .. }
            | Node::While { span, .. }
            | Node::For { span, .. }
            | Node::Match { span, .. }
            | Node::Return { span, .. }
            | Node::Break { span, .. }
            | Node::Continue { span, .. }
            | Node::Let { span, .. }
            | Node::Assign { span, .. }
            | Node::IndexAssign { span, .. }
            | Node::FnDef { span, .. }
            | Node::TypeAlias { span, .. }
            | Node::EnumDef { span, .. }
            | Node::StructDef { span, .. }
            | Node::TraitDef { span, .. }
            | Node::ImplDef { span, .. }
            | Node::Import { span, .. }
            | Node::MacroDef { span, .. }
            | Node::Perform { span, .. }
            | Node::Handle { span, .. }
            | Node::ModelDef { span, .. }
            | Node::MsgDef { span, .. }
            | Node::UpdateDef { span, .. }
            | Node::AppDef { span, .. }
            | Node::RelDef { span, .. }
            | Node::Solve { span, .. }
            | Node::PromptSection { span, .. }
            | Node::DocumentSection { span, .. }
            | Node::Observe { span, .. }
            | Node::Span { span, .. }
            | Node::Parallel { span, .. }
            | Node::Export { span, .. }
            | Node::Quasiquote { span, .. }
            | Node::Orchestrate { span, .. }
            | Node::WithConfig { span, .. }
            | Node::Sequence { span, .. }
            // v0.104.6 D472：4 类新构造
            | Node::Worker { span, .. }
            | Node::Transaction { span, .. }
            | Node::Eval { span, .. }
            | Node::Aggregate { span, .. }
            // v0.104.6 D472：管道 + commit / rollback
            | Node::Pipe { span, .. }
            | Node::Commit { span, .. }
            | Node::Rollback { span, .. }
            | Node::Expr { span, .. } => *span,
        }
    }

    /// 获取节点的 meta 引用。
    pub fn meta(&self) -> &M {
        match self {
            Node::Literal { meta, .. }
            | Node::Variable { meta, .. }
            | Node::BinaryOp { meta, .. }
            | Node::Call { meta, .. }
            | Node::MethodCall { meta, .. }
            | Node::Or { meta, .. }
            | Node::And { meta, .. }
            | Node::DynTrait { meta, .. }
            | Node::Prompt { meta, .. }
            | Node::ClosureExpr { meta, .. }
            | Node::ListLit { meta, .. }
            | Node::DictLit { meta, .. }
            | Node::Index { meta, .. }
            | Node::If { meta, .. }
            | Node::While { meta, .. }
            | Node::For { meta, .. }
            | Node::Match { meta, .. }
            | Node::Return { meta, .. }
            | Node::Break { meta, .. }
            | Node::Continue { meta, .. }
            | Node::Let { meta, .. }
            | Node::Assign { meta, .. }
            | Node::IndexAssign { meta, .. }
            | Node::FnDef { meta, .. }
            | Node::TypeAlias { meta, .. }
            | Node::EnumDef { meta, .. }
            | Node::StructDef { meta, .. }
            | Node::TraitDef { meta, .. }
            | Node::ImplDef { meta, .. }
            | Node::Import { meta, .. }
            | Node::MacroDef { meta, .. }
            | Node::Perform { meta, .. }
            | Node::Handle { meta, .. }
            | Node::ModelDef { meta, .. }
            | Node::MsgDef { meta, .. }
            | Node::UpdateDef { meta, .. }
            | Node::AppDef { meta, .. }
            | Node::RelDef { meta, .. }
            | Node::Solve { meta, .. }
            | Node::PromptSection { meta, .. }
            | Node::DocumentSection { meta, .. }
            | Node::Observe { meta, .. }
            | Node::Span { meta, .. }
            | Node::Parallel { meta, .. }
            | Node::Export { meta, .. }
            | Node::Quasiquote { meta, .. }
            | Node::Orchestrate { meta, .. }
            | Node::WithConfig { meta, .. }
            | Node::Sequence { meta, .. }
            // v0.104.6 D472：4 类新构造
            | Node::Worker { meta, .. }
            | Node::Transaction { meta, .. }
            | Node::Eval { meta, .. }
            | Node::Aggregate { meta, .. }
            // v0.104.6 D472：管道 + commit / rollback
            | Node::Pipe { meta, .. }
            | Node::Commit { meta, .. }
            | Node::Rollback { meta, .. }
            | Node::Expr { meta, .. } => meta,
        }
    }
}

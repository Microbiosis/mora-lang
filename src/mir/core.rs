//! v0.89: Core IR — 9 层架构第 3 层（函数式降维）。
//!
//! CoreInst 是 38 种 SSA 可处理的基元指令。高层抽象（闭包、ADT、
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
//! MirInst 有 59 个变体，CoreInst 有 38 个。**大部分** MirInst 变体没有
//! CoreInst 直接等价物 —— 这是设计如此，不是缺口：
//!
//! | 类别 | MirInst 变体 | Core 处理方式 |
//! |------|-------------|--------------|
//! | 声明 | TraitDef/ImplDef/EnumDef/StructDef/TypeAlias | 编译期消失，不进 Core |
//! | TEA | ModelDef/MsgDef/UpdateDef/AppDef | 编译期注册，不进 Core |
//! | I/O | Save/Load/ReadFile/WriteFile/... | RIR 层处理 |
//! | 并发 | Send/Halt | CMIR 层处理（`Aggregate` 自 D483 起有 Core 原语） |
//! | 元编程 | MacroDef | 编译期展开（`Quasiquote` 自 D483 起有 Core 原语） |
//! | 效果 | Handle (单节点) | EffectInstall + EffectRestore (拆分) |
//! | 闭包 | TaskDef/Closure | ClosureCreate + ClosureCall |
//! | 控制流 | Label/Jump/JumpIf/JumpIfNot | Branch/Jump (BlockId) |
//! | 管道 | Pipe | ClosureCall（值调用） |
//! | AI | Prompt/PromptSection/DocumentSection | Prompt / Scope |
//!
//! Core 的职责是**纯 SSA 可优化子集**，而非 MirInst 的全集映射。
//! 声明/TEA/I/O/并发/元编程等语义由各自层级处理，不经过 Core 优化。
//!
//! ### D483：补齐 6 个原语（断言 / 聚合 / 模板 / 事务 / 编排）
//!
//! `eval` / `aggregate` / quasiquote / `commit` / `rollback` / `orchestrate`
//! 此前在 Core 层**没有对应原语** ⇒ 降维只能写 `Const(dst, Nil)` 占位
//! （编排更是用**空区域**冒充）⇒ 产物里**看不见**构造，更看不见载荷。
//! D483 起每个都有原语，形状**镜像** MIR 层对应指令（与 D482 同款原则）：
//! Core 层的价值是「结构可见」，不是「比 MIR 更少」。
//!
//! ### D484：Core 层**自己的值表示**
//!
//! `ClosureCreate` 的体是 **Core IR**（`CoreFunction`）⇒ 装不进
//! `Value::Closure`（那装的是 `Arc<MirFunction>`）⇒ 语言值新增
//! `Value::CoreClosure { params, env, core_body }`（形状同款）。
//! **其余值一律复用 `Value`** ⇒ 不重复实现环境语义，也不引入转换层。

use crate::common::BinaryOp;
use crate::mir::effect::EffectRow;
use crate::mir::fcfg::{Pattern, Reg};
use crate::typeck::Type;
use crate::value::Value;

// ===================================================================
// 模式辅助
// ===================================================================

/// 模式里的**绑定名**（`x when x > 0` 里的 `x`）。
///
/// v0.104.6 D481：`EnumMatchArm.bindings` 此前**恒为空** ⇒ 模式绑定的名字
/// 在 Core 产物里**根本不存在**。
pub fn pattern_bindings(p: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    collect_pattern_bindings(p, &mut out);
    out
}

fn collect_pattern_bindings(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Variable(n) => {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        Pattern::Tuple(ps) | Pattern::List(ps) => {
            for x in ps {
                collect_pattern_bindings(x, out);
            }
        }
        Pattern::ListVec { head, tail } => {
            for x in head {
                collect_pattern_bindings(x, out);
            }
            if let Some(t) = tail {
                collect_pattern_bindings(t, out);
            }
        }
        Pattern::Dict(kvs) => {
            for (_, v) in kvs {
                collect_pattern_bindings(v, out);
            }
        }
        Pattern::TypeAscription(inner, _) => collect_pattern_bindings(inner, out),
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

/// 字面量 → 运行期值（**去掉源码位置** —— Core 产物里不留 Span 噪声）。
pub fn literal_to_value(lit: &crate::common::Literal) -> Value {
    use crate::common::Literal;
    match lit {
        Literal::Int(n, _) => Value::Int(*n),
        Literal::Float(f, _) => Value::Float(*f),
        Literal::BigInt(n, _) => Value::BigInt(n.clone()),
        Literal::String(s, _) => Value::String(s.clone()),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
        Literal::Char(c, _) => Value::Char(*c),
    }
}

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
    /// 具名调用：按**名字**分派（用户函数 / 内建）。
    ///
    /// v0.104.6 D482：此前 `Call` **不带名字** ⇒ 降维时把 `callee_name` 丢了
    /// ⇒ Core 层**根本无法执行具名调用**（分派靠名字）。补上名字后，
    /// `Call` 与 [`CoreInst::ClosureCall`] 的分工明确：
    /// `Call` 按名分派，`ClosureCall` 按寄存器里的可调用值分派。
    ///
    /// 形状**镜像** `MirInst::Call(dst, name, args)`（同样只留名字，不留
    /// callee 寄存器 —— 名字才是分派的唯一依据）。
    Call(Reg, String, Vec<Reg>),
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
    /// 调用闭包（**值调用**）：callee 寄存器持有可调用值。
    ///
    /// 形状**镜像** `MirInst::CallValue(dst, callee, args)`。
    ClosureCall(Reg, Reg, Vec<Reg>),
    /// 方法调用：receiver 寄存器 + 方法名 + 实参。
    ///
    /// v0.104.6 D482：此前方法调用被降成**具名** `Call` ⇒ 分派方式错了
    /// （方法要走 `mir_call_method`，不是 `mir_call_function`）。
    /// 形状**镜像** `MirInst::MethodCall(dst, receiver, method, args)`。
    MethodCall(Reg, Reg, String, Vec<Reg>),
    /// 模板求值：把各片段的字符串形式拼起来。
    ///
    /// v0.104.6 D482：此前被降成 `Call("prompt", …)` ⇒ 一个**不存在的函数**
    /// ⇒ 既执行不了、也误导。形状**镜像** `MirInst::Prompt(dst, parts)`。
    Prompt(Reg, Vec<Reg>),

    // ── ADT 降维 ──
    /// 枚举构造。
    EnumConstruct {
        dst: Reg,
        enum_name: String,
        variant: String,
        fields: Vec<Reg>,
    },
    /// 枚举匹配。
    ///
    /// v0.104.6 D482：补 `dst` —— 匹配**有值**（命中的那条臂的返回值），
    /// 此前 Core 的 `EnumMatch` 没有结果寄存器 ⇒ 匹配的值在产物里**丢了**。
    /// 形状**镜像** `MirInst::MatchExpr`（所有臂写同一个外层寄存器）。
    EnumMatch {
        dst: Reg,
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
        /// 区域**载荷**（按 `kind` 而异）。
        ///
        /// v0.104.6 D485：此前 `Scope` 只带 `kind` + 体 ⇒ 各 kind 的**载荷
        /// 全丢**（`with_config` 的绑定、`span` / `observe` 的名字与标签、
        /// `export` 的名字）⇒ 区域在 Core 层「**看得见但执行不了**」。
        payload: ScopePayload,
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

    // ── 断言 / 聚合 / 模板 / 事务 / 编排（D483）──
    //
    // 这 6 类构造此前在 Core 层**没有对应原语** ⇒ 只能写 `Const(dst, Nil)`
    // 占位（编排更是用**空区域**冒充）⇒ 产物里**看不见**它们，更看不见它们
    // 的载荷（断言名 / 聚合器名 / 模板片段 / 编排图）。
    //
    // D483 起它们各自有原语，形状**镜像** MIR 层对应指令（与 D482 同款原则）：
    // Core 层的价值是「结构可见」，不是「比 MIR 更少」。
    //
    // **语句的值**契约（与 MIR 层**逐字**一致，改动前先读 MIR 侧注释）：
    // - `eval` 的值 = **given 表达式的值**（不是 Nil）；
    // - `aggregate` 的值 = **被贡献表达式的值**（不是 Nil）；
    // - `commit` / `rollback` / `orchestrate` 是 unit 语句 ⇒ 值恒为 Nil。
    //
    // ⚠ 此前 Core 层给 `eval` / `aggregate` 补的 `Const(dst, Nil)` 会**覆盖**
    // 那个寄存器（`dst` 与 `given` / `value` **同一个**寄存器）⇒ 把语句的值
    // 变成了 Nil，与 MIR 层分叉。D483 起不再补。
    /// 断言求值（`eval` 语句）—— 镜像 `MirInst::Eval`。
    Eval {
        /// 断言名（`eval "n" ...` 的 `"n"`；未给名字时是空串）。
        name: String,
        /// 被断言的表达式所在寄存器（**语句的值 = 它的值**）。
        given: Reg,
        /// 期望值所在寄存器（逐一比较；任一不等 ⇒ 运行期报错）。
        expects: Vec<Reg>,
        /// 数值容差（`None` ⇒ 精确比较）。
        tolerance: Option<f64>,
        /// 回放路径（**仅记录用**，执行层不消费）。
        replay_path: Option<String>,
    },
    /// 向聚合器贡献一个值（`aggregate`）—— 镜像 `MirInst::Aggregate`。
    Aggregate {
        /// 聚合器名。
        name: String,
        /// 被贡献的值所在寄存器（**语句的值 = 它的值**）。
        value: Reg,
    },
    /// quasiquote —— 镜像 `MirInst::Quasiquote`。
    ///
    /// 片段类型**复用** `crate::mir::QuasiquoteSegment`（不另立一份）：
    /// 片段是「静态文字 + 寄存器引用」的混合序列，执行语义由 `h_quasiquote`
    /// 唯一定义 ⇒ 复用它才能让 Core 执行器与 MIR 执行器**逐字同一套**拼接规则。
    Quasiquote {
        dst: Reg,
        segments: Vec<crate::mir::QuasiquoteSegment>,
    },
    /// 事务提交 —— 镜像 `MirInst::Commit`（unit 语句，值恒为 Nil）。
    Commit,
    /// 事务回滚 —— 镜像 `MirInst::Rollback`（unit 语句，值恒为 Nil）。
    Rollback,
    /// 编排 —— 镜像 `MirInst::Orchestrate`。
    ///
    /// 载荷**原样**承载 witness 形态（与 `Node::Orchestrate` 同款，理由见
    /// D473）：编排是**数据构造语句**（不参与控制流），其载荷是**数据**而非
    /// CFG 结构 ⇒ 原样透传，不做有损投影。
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: crate::mir::witness::WitnessOrchestrateKind,
    },
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
    /// 分支的**模式**（源码文本形式，与 `MirInst::MatchArmInst` 同形）。
    ///
    /// v0.104.6 D482：此前是**只读标签** `variant: String`（而且是
    /// `format!("{:?}", pattern)` 的 Debug 串，连 `Span` 都进去了）⇒
    /// 只能给人看，**没法执行**（匹配要模式本身）。
    ///
    /// 现在存的是**模式源码文本**（`3` / `x` / `{a: 1}`）⇒ 执行器可直接
    /// 复用 `self_match_pattern` ⇒ 与 MIR 层**逐字同一套**匹配语义。
    pub pattern: String,
    /// 模式里的绑定名（`x when x > 0` 里的 `x`）。
    ///
    /// v0.104.6 D481：此前**恒为空** ⇒ 模式绑定的名字在 Core 产物里
    /// **根本不存在**。
    pub bindings: Vec<String>,
    /// 守卫（`when` 条件）—— **独立代码区**（在自己的寄存器空间里求值）。
    ///
    /// v0.104.6 D482：此前 Core 的 arm **不带守卫** ⇒ `when` 条件在产物里
    /// 完全消失（`x when x > 0` 退化成 `x`）。守卫的求值时机与 MIR 层一致：
    /// **模式绑定之后**（守卫体因此能读到绑定变量）。
    pub guard: Option<CoreFunction>,
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

/// 区域**载荷**（按 [`CoreInst::Scope`] 的 `kind` 而异）。
///
/// v0.104.6 D485：`Scope` 此前只带 `kind` + 体 ⇒ 各 kind 自己的数据无处安放
/// ⇒ 执行器**无法**忠实复现（MIR 侧这些数据在 handler 的实参里）。
///
/// 载荷是**数据**（不是 CFG 结构）⇒ 与 D473「编排载荷原样透传」同款理由。
#[derive(Debug, Clone)]
pub enum ScopePayload {
    /// 无额外载荷（`worker` / `parallel` / `transaction` …）。
    None,
    /// `with_config`：绑定名 + 值所在寄存器。
    ///
    /// ⚠ 值在**外层**寄存器空间（配置绑定在外层求值，块体是独立代码区）。
    WithConfig(Vec<(String, Reg)>),
    /// `span` / `observe`：span 名 + 属性（`observe` 的 kind 作名前缀）。
    Trace {
        name: String,
        tags: Vec<(String, String)>,
    },
    /// `export`：被导出的名字（声明在体**之后**标记，与 MIR 层同序）。
    Export(Vec<String>),
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

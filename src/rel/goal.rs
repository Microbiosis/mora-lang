//! 关系引擎的目标代数与项语言（声明式范式 Foundation 层）。
//!
//! 本模块定义逻辑式编程的三类编译期/运行期数据：
//!
//! - [`Term`]：项模板。运行期形态是 `Val(Value)`（叶子可含
//!   `Value::LogicVar`）；编译期子句模板额外允许 `Param(i)` 参数槽与
//!   `Cons`/`List`/`Dict` 构造形态（供 `rel` 规则头/体在 rename 时
//!   展开为运行期 Value 树）。
//! - [`Goal`]：目标。目标是一等值（见 `Value::Goal`），可经
//!   `unify`/`both`/`either`/`fail`/`succeed` 内建与关系调用在运行期构造。
//! - [`Clause`]：关系子句（事实 = body 为 `Succeed` 的子句）。
//!
//! 搜索语义见 [`super::search`]；合一语义见 [`super::unify`]。

use std::sync::Arc;

use crate::value::Value;

/// 项模板。
///
/// 运行期目标只含 `Val`（其 Value 树叶子可为 `Value::LogicVar`）；
/// 编译期子句（`Clause`）可含 `Param(i)` 与构造形态，由
/// [`super::search`] 在子句调用时 rename 为 fresh 逻辑变量。
#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    /// 值项：任意 Value 树，叶子可含 `Value::LogicVar`。
    Val(Value),
    /// 子句参数槽（编译期）。rename 时绑定 fresh 逻辑变量。
    Param(usize),
    /// cons 单元构造（编译期模板）。与 `Value::Cons` 同构。
    Cons(Box<Term>, Box<Term>),
    /// 列表构造（编译期模板）。与 `Value::List` 同构。
    List(Vec<Term>),
    /// 字典构造（编译期模板）。与 `Value::Dict` 同构。
    Dict(Vec<(String, Term)>),
}

/// 目标。一等值（`Value::Goal`），可组合、可高阶传递。
#[derive(Debug, Clone, PartialEq)]
pub enum Goal {
    /// 恒真目标。
    Succeed,
    /// 恒假目标。
    Fail,
    /// 合一目标：两树合一成功则继续，失败则剪枝。
    Unify(Term, Term),
    /// 合取：顺序执行，任一失败整枝失败。
    Conj(Vec<Goal>),
    /// 析取：各备选与其他待定任务交错调度（见 `search`）。
    Disj(Vec<Goal>),
    /// 关系调用。
    /// `clauses` 为 `Some` 时自含子句（由 `Value::Relation` 调用构造，
    /// 目标值自包含）；为 `None` 时由搜索期宿主按名解析（编译期子句体
    /// 中的递归调用形态）。
    Invoke {
        name: String,
        clauses: Option<Arc<Vec<Clause>>>,
        args: Vec<Term>,
    },
    /// 宿主计算投影：调用宿主函数（确定性求值），结果与 `result` 合一。
    /// 实参必须完全绑定（不含未解析逻辑变量），否则该搜索分支报错。
    Project {
        func: ProjectFn,
        args: Vec<Term>,
        result: Term,
    },
}

/// 投影的宿主函数来源。
///
/// - `Name`：编译期子句体中的具名引用（`rel r(x, y) project(f, x, y) end`），
///   搜索期由宿主按名从环境解析 → 可调用值。
/// - `Value`：运行期已求值的可调用值（solve 目标体内 `project(f, ...)` 的
///   构建产物，`f` 已由目标构建体求值）。
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectFn {
    Name(String),
    Value(Value),
}

/// 关系子句。事实 = `body == Goal::Succeed`。
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    /// 子句局部变量名表（索引 = `Term::Param` 槽位）。
    /// 含头部参数、体独有变量（如递归规则里的中间变量）与匿名变量
    /// （`__anonN`）；每次调用 rename 为 fresh 逻辑变量。
    pub params: Vec<String>,
    /// 头项模板（与调用实参逐一合一）。
    pub head: Vec<Term>,
    /// 体目标。
    pub body: Goal,
}

impl Clause {
    /// 事实子句构造器。
    pub fn fact(params: Vec<String>, head: Vec<Term>) -> Clause {
        Clause { params, head, body: Goal::Succeed }
    }

    /// 规则子句构造器。
    pub fn rule(params: Vec<String>, head: Vec<Term>, body: Goal) -> Clause {
        Clause { params, head, body }
    }
}

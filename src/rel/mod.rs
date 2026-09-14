//! 声明式编程范式（逻辑式/关系式）引擎 —— Foundation 层。
//!
//! v0.102 融入。底层原语：
//!
//! 1. **关系**（[`Clause`] 集合，一等值 `Value::Relation`）—— 描述「什么成立」；
//! 2. **逻辑变量 + 替换**（[`Subst`]，HAMT 持久映射，O(1) 快照使回溯零成本）；
//! 3. **目标代数**（[`Goal`]，一等值 `Value::Goal`，可组合可高阶）；
//! 4. **交错搜索**（[`Search`]，FIFO 任务队列状态机，回溯内嵌引擎数据流，
//!    不依赖 VM 续延）。
//!
//! 层级约束：本模块只依赖 `crate::value`；宿主回调（关系解析 / 投影执行）
//! 经 [`RelHost`] trait 由解释器层注入。
//!
//! 终止性模型与 Prolog 相同：递归关系的终止由关系作者负责（交错调度保证
//! 公平性但不保证终止）；`solve N` 的界形式可安全采样潜在无限解流。

pub mod goal;
pub mod reify;
pub mod search;
pub mod subst;
pub mod unify;

pub use goal::{Clause, Goal, ProjectFn, Term};
pub use reify::reify;
pub use search::{RelHost, Search};
pub use subst::Subst;
pub use unify::unify;

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
//! **两层界限**（v0.104.6 D397 起）：
//!
//! 1. **产解数**：`solve N` 的 `limit` 检查**已收集的解数量**
//!    （`solutions.len() >= cap`），位于 `h_solve` 循环内、两次
//!    `next_solution` 之间 —— 它限制「产出多少个解」。
//! 2. **搜索工作量**：[`Search`] 自带**步数上界**（`DEFAULT_MAX_STEPS`，
//!    可用 `Search::with_max_steps` 调整、传 `0` 回到无界）—— 它限制
//!    「搜索多久」。左递归规则（`rel loop2(x) loop2(x) end`）会让队列永不
//!    排空，**若无上界则 `next_solution` 永不返回**，`limit` 也就永远
//!    轮不到被检查 —— 两者是**互补**的两层，缺一不可。
//!
//! 实测（真实 CLI）：左递归 `solve 2 { loop2(?X) }` 现在于
//! `DEFAULT_MAX_STEPS`（20 万步）处**明确报错**退出（约 1.8 秒），
//! 报「fuel exhausted」并点名 `max_steps`；而**合法**的传递闭包
//! （`rel_basic.mora` 全解）只需几十步，毫秒级完成。
//!
//! ⚠ 因此**「终止性完全由关系作者负责」已不再准确**：对**产不出解**的规则，
//! 搜索会撞上步数上界并报错，而不是无限挂死。这是为了把「静默挂死」换成
//! 「明确报错」而**有意付出的代价**（超长但合法的搜索需调大预算）。
//! 公平交错（`Disj` 分支进队尾）保证**不饿死**右侧备选，这条性质未变。

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

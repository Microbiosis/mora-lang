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
//! 公平性但不保证终止）。
//!
//! ⚠ v0.104.6 D397 更正：原文接下来说「`solve N` 的界形式可安全采样潜在无限
//! 解流」—— **这半句对「无限搜索」不成立**。
//!
//! `solve N` 的 `limit` 只在 `h_solve` 的循环里检查**已收集的解数量**
//! （`solutions.len() >= cap`），而那是在**两次 `next_solution` 之间**。
//! 左递归规则（`rel loop2(x) loop2(x) end`）会让 `next_solution` 的队列
//! **永不排空** ⇒ 该调用**永不返回** ⇒ `limit` **永远轮不到被检查**。
//!
//! 实测（真实 CLI，`solve` 无界与 `solve 2` **两者都挂死**，20s 不退出、
//! 无报错、无输出，只能手动杀进程）：
//!
//! ```mora
//! rel loop2(x) loop2(x) end
//! solve 2 { loop2(?X) }     -- 仍然挂死
//! ```
//!
//! ⇒ 准确表述：**`solve N` 能限制「产出多少个解」，不能限制「搜索多久」**。
//! 「终止由关系作者负责」是**明文的设计取舍**（Prolog 模型），**本轮不擅改**；
//! 但是否加搜索步数上界 / 左递归检测，属**产品策略决定**，
//! 已列入待裁决（详见 CHANGELOG D397）。

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

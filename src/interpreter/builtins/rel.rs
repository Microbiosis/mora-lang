//! v0.102: 声明式范式（逻辑式/关系式）运行时目标原语。
//!
//! 这些 builtin 在**目标构建期**运行（solve 的 goal 构建体内），把运行时
//! 值提升为 [`Goal`]/[`Term`]，供引擎执行搜索：
//!
//! - `unify(a, b)` → `Goal::Unify`（两值合一）
//! - `both(g1, g2, ...)` → `Goal::Conj`（合取；亦接受单个 Goal 列表）
//! - `either(g1, g2, ...)` → `Goal::Disj`（析取；亦接受单个 Goal 列表）
//! - `project(f, a1, ..., an, result)` → `Goal::Project`（宿主计算投影）
//! - `fail` / `succeed` → 常量目标
//!
//! 关系值（`Value::Relation`）作为可调用值经 [`Interpreter::call_value`]
//! 构造 `Goal::Invoke`（自含子句，搜索期无需按名再解析）——见
//! `interpreter::dispatch::call_value` 的 Relation 分支。

use super::*;
use crate::rel::{Goal, ProjectFn, Term};

impl Interpreter {
    /// 把实参值提升为项模板。逻辑变量（`Value::LogicVar`）原样保留 ——
    /// 引擎在搜索期沿替换解析。
    fn value_to_term(v: &Value) -> Term {
        Term::Val(v.clone())
    }

    /// 从实参列表取目标序列：若单个实参是 List 且元素全为 Goal，则展开
    /// 为多目标（`both([g1, g2])` 形态）；否则逐个要求 Goal。
    fn goals_from_args(args: &[Value], fname: &str) -> Result<Vec<Goal>, String> {
        if args.len() == 1
            && let Value::List(items) = &args[0]
            && !items.is_empty()
            && items.iter().all(|i| matches!(i, Value::Goal(_)))
        {
            return Ok(items
                .iter()
                .map(|i| match i {
                    Value::Goal(g) => (**g).clone(),
                    _ => unreachable!("已由 all() 判定"),
                })
                .collect());
        }
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Value::Goal(g) => out.push((**g).clone()),
                other => {
                    return Err(format!(
                        "{} 的目标实参必须是 goal 值，得到 {}",
                        fname,
                        crate::flow::type_name(other)
                    ));
                }
            }
        }
        Ok(out)
    }

    /// `unify(a, b)` — 两值合一目标。
    pub(crate) fn call_builtin_unify(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != 2 {
            return Err(format!("unify(a, b) 期望 2 个实参，得到 {}", args.len()));
        }
        Ok(Value::Goal(Box::new(Goal::Unify(
            Self::value_to_term(&args[0]),
            Self::value_to_term(&args[1]),
        ))))
    }

    /// `both(g1, g2, ...)` — 合取（顺序执行）。
    pub(crate) fn call_builtin_both(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let goals = Self::goals_from_args(&args, "both")?;
        if goals.is_empty() {
            return Err("both 至少需要一个目标".to_string());
        }
        Ok(Value::Goal(Box::new(Goal::Conj(goals))))
    }

    /// `either(g1, g2, ...)` — 析取（交错搜索的备选分支）。
    pub(crate) fn call_builtin_either(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let goals = Self::goals_from_args(&args, "either")?;
        if goals.is_empty() {
            return Err("either 至少需要一个目标".to_string());
        }
        Ok(Value::Goal(Box::new(Goal::Disj(goals))))
    }

    /// `project(f, a1, ..., an, result)` — 宿主计算投影。
    /// 末位实参是与投影结果合一的目标项，其余是投影实参。
    pub(crate) fn call_builtin_project(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err(format!(
                "project(f, args..., result) 期望至少 2 个实参，得到 {}",
                args.len()
            ));
        }
        let func = args[0].clone();
        if !matches!(
            func,
            Value::Closure { .. } | Value::Task { .. } | Value::Builtin(_)
        ) {
            return Err(format!(
                "project 的第一个实参必须是可调用值，得到 {}",
                crate::flow::type_name(&func)
            ));
        }
        let result = args.last().cloned().unwrap_or(Value::Nil);
        let proj_args: Vec<Term> = args[1..args.len() - 1]
            .iter()
            .map(Self::value_to_term)
            .collect();
        Ok(Value::Goal(Box::new(Goal::Project {
            func: ProjectFn::Value(func),
            args: proj_args,
            result: Self::value_to_term(&result),
        })))
    }

    /// `fail` — 恒假目标。
    pub(crate) fn call_builtin_fail(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("fail 不接受实参".to_string());
        }
        Ok(Value::Goal(Box::new(Goal::Fail)))
    }

    /// `succeed` — 恒真目标。
    pub(crate) fn call_builtin_succeed(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("succeed 不接受实参".to_string());
        }
        Ok(Value::Goal(Box::new(Goal::Succeed)))
    }
}

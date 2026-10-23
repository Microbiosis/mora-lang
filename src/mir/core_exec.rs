//! v0.104.6 D482：Core 层的**执行器** —— 让 Core IR 从「影子」变成「可执行」。
//!
//! ## 为什么
//!
//! Core 层此前只是**影子**（`probe_all_layers` 只统计指令数）⇒ 降维里的任何
//! 错误都不会被**执行**发现，只能靠人工审。本执行器给 Core 层一条**独立**
//! 的执行路径 ⇒ 可以与 MIR 执行器**对拍**（同一程序、两个执行器、结果必须
//! 一致）—— 这正是 D474 退役差分机制时缺的**语义级**对照：当年的差分
//! **只比形状、不比语义**（`or`/`and` 短路不赋值就是两边一起错、差分抓不到）。
//!
//! ## 可执行子集（v1）
//!
//! 支持：常量 / 拷贝 / 二元运算 / 环境读写 / 列表 / 字典 / 索引 / 索引赋值 /
//! 分支 / 跳转 / 返回 / 具名调用 / 值调用 / 方法调用 / 模板 / match（含守卫）/
//! **断言（eval）/ 聚合（aggregate）/ quasiquote / 事务提交（commit）**。
//!
//! **不支持**（遇到就**响亮报错**，绝不静默降级）：
//!
//! - **闭包 / task**（`ClosureCreate`）—— Mora 的 `Value::Closure` 装的是
//!   `Arc<MirFunction>`，装不下 Core 闭包 ⇒ Core 需要自己的值表示，是独立的
//!   一批；
//! - **区域构造**（`Scope`）—— Core 的 `Scope` 只是「区域」，**并发 / 事务 /
//!   可观测性语义在执行层** ⇒ 把它顺序执行会与 MIR 层**静默分叉**
//!   （所以这里宁可报错，也不假装能跑）；
//! - **编排**（`Orchestrate`）—— agent 图 / BSP 超步是执行层的语义（同上）；
//! - **效果**（`EffectPerform` / `EffectInstall` / `EffectRestore`）—— 需要
//!   handler 栈与 `resume` 语义；
//! - **ADT 构造 / thunk** —— Core 层当前不产出这些指令。
//!
//! ## 语义基线
//!
//! 与 MIR 层**同源**：环境用 Mora 的 `Environment`（持久 map + 父链）、
//! 模式匹配复用 `self_match_pattern`、内建分派走 `MirHost`、
//! 断言 / 聚合 / quasiquote 复用 `h_eval` / `h_aggregate` / `h_quasiquote`
//! ⇒ 差分覆盖的是**降维与控制流**，不是内建本身。

use crate::mir::core::{CoreFunction, CoreInst, CoreTerminator};
use crate::mir::effect::Effects;
use crate::mir::handlers::{
    h_assign, h_binary_op, h_const, h_define, h_dict_lit, h_index, h_index_assign, h_list_lit,
    h_prompt, h_var,
};
use crate::mir::host::MirHost;
use crate::value::{Environment, Value};

/// 执行一个 CoreFunction，返回最后的值。
///
/// 契约与 MIR 层 `mir::vm::execute` 一致：**最后一个写寄存器的指令的值**；
/// `Return(Some(r))` 则返回该寄存器的值，`Return(None)` 返回末值。
pub fn execute(
    func: &CoreFunction,
    env: &mut Environment,
    interp: &mut dyn MirHost,
    effects: &mut Effects,
) -> Result<Value, String> {
    let mut regs = vec![Value::Nil; func.n_regs.max(1)];
    let mut pc = func.entry;
    let mut last = Value::Nil;
    // 步数上限：Core 函数不该有无限循环（与 MIR 引擎的 wave 上限同思路）——
    // 无上限的话，一个坏掉的分支目标会让测试**挂死**而不是报错。
    let mut steps: u64 = 0;
    let max_steps: u64 = 10_000_000;
    loop {
        steps += 1;
        if steps > max_steps {
            return Err(format!(
                "Core: 执行步数超过上限 {max_steps}（分支目标可能指错）"
            ));
        }
        let block = func.blocks.get(pc).ok_or_else(|| {
            format!(
                "Core: 跳转目标块 {pc} 不存在（只有 {} 个块）",
                func.blocks.len()
            )
        })?;
        for inst in &block.insts {
            if let Some(v) = exec_inst(inst, &mut regs, env, interp, effects)? {
                last = v;
            }
        }
        match &block.terminator {
            CoreTerminator::Return(None) => return Ok(last),
            CoreTerminator::Return(Some(r)) => return reg(&regs, *r),
            CoreTerminator::Jump(t) => pc = *t,
            CoreTerminator::Branch {
                cond,
                true_bb,
                false_bb,
            } => {
                let c = reg(&regs, *cond)?;
                pc = if crate::flow::is_truthy(&c) {
                    *true_bb
                } else {
                    *false_bb
                };
            }
            CoreTerminator::Unreachable => {
                return Err("Core: 执行到 Unreachable 块（分支目标未回填？）".to_string());
            }
        }
    }
}

/// 读寄存器（越界 ⇒ 响亮报错，不静默取 Nil）。
fn reg(regs: &[Value], r: usize) -> Result<Value, String> {
    regs.get(r)
        .cloned()
        .ok_or_else(|| format!("Core: 寄存器 {r} 越界（只有 {} 个）", regs.len()))
}

/// 执行一条指令，返回它写下的值（无值 ⇒ `None`）。
fn exec_inst(
    inst: &CoreInst,
    regs: &mut [Value],
    env: &mut Environment,
    interp: &mut dyn MirHost,
    effects: &mut Effects,
) -> Result<Option<Value>, String> {
    match inst {
        CoreInst::Const(dst, v) => {
            h_const(regs, *dst, v);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::Var(dst, name) => {
            h_var(regs, *dst, name, env);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::EnvLoad(dst, name) => {
            h_var(regs, *dst, name, env);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::Copy(dst, src) => {
            let v = reg(regs, *src)?;
            regs[*dst] = v.clone();
            Ok(Some(v))
        }
        CoreInst::BinaryOp(dst, l, op, r) => {
            h_binary_op(regs, *dst, *l, op, *r)?;
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::EnvStore(name, src) => {
            let v = reg(regs, *src)?;
            h_define(interp, env, name, regs, *src);
            Ok(Some(v))
        }
        CoreInst::EnvMutate(name, src) => {
            let v = reg(regs, *src)?;
            h_assign(interp, env, name, regs, *src);
            Ok(Some(v))
        }
        CoreInst::ListLit(dst, items) => {
            h_list_lit(regs, *dst, items);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::DictLit(dst, entries) => {
            h_dict_lit(regs, *dst, entries);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::Index(dst, obj, idx) => {
            h_index(regs, *dst, *obj, *idx)?;
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::IndexAssign(obj, idx, val) => {
            h_index_assign(regs, *obj, *idx, *val)?;
            Ok(None)
        }
        CoreInst::Call(dst, name, args) => {
            let arg_vals = arg_values(regs, args)?;
            let v = interp.mir_call_function(name, arg_vals, env, effects)?;
            regs[*dst] = v.clone();
            Ok(Some(v))
        }
        CoreInst::ClosureCall(dst, callee, args) => {
            let callee_val = reg(regs, *callee)?;
            let arg_vals = arg_values(regs, args)?;
            let v = interp.call_value(&callee_val, arg_vals, effects)?;
            regs[*dst] = v.clone();
            Ok(Some(v))
        }
        CoreInst::MethodCall(dst, receiver, method, args) => {
            let obj = reg(regs, *receiver)?;
            let arg_vals = arg_values(regs, args)?;
            let v = interp.mir_call_method(obj, method, arg_vals, effects)?;
            regs[*dst] = v.clone();
            Ok(Some(v))
        }
        CoreInst::Prompt(dst, parts) => {
            h_prompt(regs, *dst, parts);
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::Expr(r) => {
            // 表达式语句：值被丢弃，但**读一次**（寄存器越界要报错）。
            let _ = reg(regs, *r)?;
            Ok(None)
        }
        // ── v0.104.6 D483：断言 / 聚合 / 模板 / 事务 ──
        //
        // 语义**复用** MIR 层的 handler（唯一来源）⇒ 与 MIR 执行器逐字同一套
        // 规则（比较 / 容差 / 失败文案 / 拼接 / 展平），不另写一份。
        CoreInst::Eval {
            name,
            given,
            expects,
            tolerance,
            ..
        } => {
            crate::mir::handlers::h_eval(regs, env, name, *given, expects, tolerance)?;
            // 语句**不写**寄存器（MIR 层 `MirInst::Eval` 的 `dst()` 也是 None）
            // ⇒ 不更新 `last`。
            Ok(None)
        }
        CoreInst::Aggregate { name, value } => {
            // 贡献进 `Effects` 数据通道（与 MIR 层 dispatch 同款：纯生产者，
            // 由引擎侧在 worker 边界归约）。
            let effect = crate::mir::handlers::h_aggregate(regs, *value, name);
            effects.push(effect);
            Ok(None)
        }
        CoreInst::Quasiquote { dst, segments } => {
            crate::mir::handlers::h_quasiquote(regs, *dst, segments)?;
            Ok(Some(regs[*dst].clone()))
        }
        CoreInst::Commit => Ok(None),
        // 回滚在 MIR 层是**运行期错误**（`MirInst::Rollback => Err(...)`）
        // ⇒ Core 侧必须同样报错，不能「当成空操作放过去」。
        CoreInst::Rollback => Err("Transaction rolled back".to_string()),
        CoreInst::EnumMatch {
            dst,
            scrutinee,
            arms,
        } => {
            let val = reg(regs, *scrutinee)?;
            let mut matched = false;
            for arm in arms {
                // 模式匹配复用 MIR 层的 `self_match_pattern` ⇒ 与 MIR 层
                // **逐字同一套**语义（它还会把绑定变量 define 进 env）。
                if !crate::mir::vm::self_match_pattern(&val, &arm.pattern, None, env) {
                    continue;
                }
                // 守卫在**模式绑定之后**求值（与 MIR 层 h_match_expr 同序）——
                // 守卫体因此能读到绑定变量。
                if let Some(guard) = &arm.guard {
                    let g = execute(guard, env, interp, effects)?;
                    if !crate::flow::is_truthy(&g) {
                        continue;
                    }
                }
                let result = execute(&arm.body, env, interp, effects)?;
                regs[*dst] = result;
                matched = true;
                break;
            }
            if !matched {
                regs[*dst] = Value::Nil;
            }
            Ok(Some(regs[*dst].clone()))
        }
        // 分支 / 跳转 / 返回的**指令形态**：控制流由**终结指令**决定，
        // 指令形态只是冗余副本（MIR 引擎里 `Flow::Jump(_)` 同样是 no-op）⇒ 忽略。
        CoreInst::Branch { .. } | CoreInst::Jump(_) | CoreInst::Return(_) => Ok(None),
        // ── v1 不支持的部分：响亮报错，不静默降级 ──
        CoreInst::ClosureCreate { .. } => Err(
            "Core 执行器（v1）**不支持闭包 / task** —— Mora 的 `Value::Closure` \
             装的是 `Arc<MirFunction>`，装不下 Core 闭包（需要 Core 自己的值表示）。"
                .to_string(),
        ),
        CoreInst::Scope { kind, .. } => Err(format!(
            "Core 执行器（v1）**不支持区域构造** `{kind}` —— 并发 / 事务 / 可观测性 \
             语义在执行层，顺序执行它会与 MIR 层静默分叉。"
        )),
        CoreInst::EffectPerform { .. }
        | CoreInst::EffectInstall { .. }
        | CoreInst::EffectRestore { .. } => {
            Err("Core 执行器（v1）**不支持效果**（需要 handler 栈与 resume 语义）。".to_string())
        }
        CoreInst::ThunkCreate { .. } | CoreInst::ThunkForce(_, _) => {
            Err("Core 执行器（v1）**不支持 thunk**。".to_string())
        }
        // 编排：agent 图 / BSP 超步是**执行层**的语义 ⇒ 顺序执行它会与 MIR 层
        // 静默分叉 ⇒ 宁可报错，也不假装能跑。
        //
        // ⚠ D483 起 Core 层有了编排**原语**（模型忠实），但执行器仍不支持
        // **执行**它 —— 「模型里有」与「执行器能跑」是**两件事**，别混。
        CoreInst::Orchestrate { .. } => Err(
            "Core 执行器（v1）**不支持编排执行** —— agent 图 / BSP 超步是执行层的语义，\
             顺序执行它会与 MIR 层静默分叉。"
                .to_string(),
        ),
        CoreInst::EnumConstruct { .. }
        | CoreInst::StructConstruct { .. }
        | CoreInst::StructAccess(_, _, _) => {
            Err("Core 执行器（v1）**不支持 ADT 构造 / 字段访问**。".to_string())
        }
        CoreInst::Phi(_, _) => Err(
            "Core 执行器（v1）**不支持 phi 节点**（Core 层由 SSA 前的分支汇合承担）。".to_string(),
        ),
        CoreInst::Unreachable => Err("Core: 不可达指令".to_string()),
    }
}

/// 实参值列表。
fn arg_values(regs: &[Value], args: &[usize]) -> Result<Vec<Value>, String> {
    args.iter().map(|r| reg(regs, *r)).collect()
}

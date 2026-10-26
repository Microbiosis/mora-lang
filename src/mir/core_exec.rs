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
//! **断言（eval）/ 聚合（aggregate）/ quasiquote / 事务提交（commit）**/
//! **闭包**（D484：Core 层有自己的值表示 `Value::CoreClosure`）/
//! **区域构造**（D485：`transaction` / `worker` / `parallel` / `export` /
//! `with_config` / `span` / `observe`）/ **代数效果**（D485：`EffectInstall` /
//! `EffectPerform` / `EffectRestore`）/ **编排执行**（D485）。
//!
//! **不支持**：**没有** —— D487 删掉了 9 个**零生产者的死变体**（ADT 构造 /
//! 字段访问 / thunk / phi / `Var` / 指令形态的 `Return`·`Unreachable`）⇒
//! 本执行器覆盖 Core 模型的**每一个**变体（编译器保证 match 穷尽且无报错
//! 分支；守卫 `tests/core_no_dead_variants.rs` 保证没有「有指令但没人发」）。
//!
//! > **task 不在不支持清单里**（D485 更正）：MIR 侧有 `MirInst::TaskDef`，但
//! > FCFG 的 `Node::FnDef` **同时**覆盖 `fn` 与 `task` 声明 ⇒ Core 层把 task
//! > 降成**具名闭包**（`ClosureCreate` + `EnvStore`）⇒ 已被 D484 的闭包支持
//! > 覆盖。此前把 task 列为「不支持」是**假命题**。
//!
//! ## 语义基线
//!
//! 与 MIR 层**同源**：环境用 Mora 的 `Environment`（持久 map + 父链）、
//! 模式匹配复用 `self_match_pattern`、内建分派走 `MirHost`、
//! 断言 / 聚合 / quasiquote 复用 `h_eval` / `h_aggregate` / `h_quasiquote`、
//! 区域语义逐条对齐 `h_transaction` / `h_worker` / `h_parallel` / `h_span` /
//! `h_observe` / `h_with_config`、编排复用 `h_orchestrate`
//! ⇒ 差分覆盖的是**降维与控制流**，不是内建本身。
//!
//! ## Core 层自己的值表示（D484）
//!
//! `Value::Closure` 装的是 `Arc<MirFunction>`（**MIR** 的函数体）⇒ 装不下
//! Core 闭包（函数体是 Core IR 的 `CoreFunction`）。
//!
//! ⇒ D484 给语言值加了 `Value::CoreClosure { params, env, core_body }`
//! （形状与 `Value::Closure` 同款）。**其余值一律复用 `Value`** —— Core
//! 执行器仍用 Mora 的 `Environment` 与既有 handler ⇒ 不重复实现环境语义，
//! 也不引入 `Value ↔ CoreValue` 的转换层。

use crate::mir::core::{CoreFunction, CoreInst, CoreTerminator, ScopePayload};
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
    // v0.104.6 D485：效果 handler 的**旧值暂存栈**。
    //
    // Core 模型把 `handle` 拆成 `EffectInstall` → 体 → `EffectRestore` 三条
    // 指令（体**内联**在外层 CFG 里）⇒ install 与 restore 之间必须记住被
    // 换下的旧 handler（嵌套 handle 要能逐层恢复）⇒ 需要一处栈。
    //
    // 为什么**每次 execute 一个新栈**就够：install/restore 在降维里**总是**
    // 同一个动态范围内配对（体是内联的）⇒ 任何嵌套代码区（闭包体 / 区域体 /
    // 守卫体）自带一个新栈，它自己的 install/restore 依然自洽；而 perform
    // 找的是**宿主**的 handler 注册表（不在这栈上）⇒ 外层的 handler 照样
    // 可见。
    let mut saved_handlers: Vec<(
        String,
        Option<Box<dyn crate::runtime::effect::EffectHandler>>,
    )> = Vec::new();
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
            if let Some(v) = exec_inst(inst, &mut regs, env, interp, effects, &mut saved_handlers)?
            {
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
#[allow(clippy::too_many_arguments)]
fn exec_inst(
    inst: &CoreInst,
    regs: &mut [Value],
    env: &mut Environment,
    interp: &mut dyn MirHost,
    effects: &mut Effects,
    saved_handlers: &mut Vec<(
        String,
        Option<Box<dyn crate::runtime::effect::EffectHandler>>,
    )>,
) -> Result<Option<Value>, String> {
    match inst {
        CoreInst::Const(dst, v) => {
            h_const(regs, *dst, v);
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
            // v0.104.6 D486：走 **`h_call`**（MIR 侧 `MirInst::Call` 的 dispatch
            // **同一份**）⇒ 解析顺序逐字一致：
            // ① 任务注册表（MIR 侧有；**Core 侧 task 降成了闭包** ⇒ 恒空）；
            // ② 环境里绑了 Task / Closure / Compose / Partial / **Relation** /
            //    CoreClosure ⇒ 直接值调用；
            // ③ 其余 → 宿主 `mir_call_function`（内建分派）。
            //
            // ⚠ 此前 Core 侧**直接**调 `mir_call_function` ⇒ **跳过了 ②** ⇒
            // ① 关系值（`rel edge(...)`）不可调用 ⇒ `solve` 的目标构建体失败；
            // ② 用户函数**遮蔽内建**时与 MIR 层分叉（MIR 走 ②，Core 走 ③）。
            //
            // 任务注册表恒空：Core 层把 `task` 降成**具名闭包** ⇒ 走 ② 的
            // `Value::CoreClosure` 分支（D484）⇒ 语义等价。
            let task_registry: std::collections::HashMap<
                &str,
                (&[String], &crate::mir::MirFunction),
            > = std::collections::HashMap::new();
            crate::mir::handlers::h_call(
                regs,
                *dst,
                name,
                args,
                &task_registry,
                interp,
                env,
                effects,
            )?;
            Ok(Some(regs[*dst].clone()))
        }
        // v0.104.6 D484：创建 **Core 闭包**。
        CoreInst::ClosureCreate {
            dst, params, body, ..
        } => {
            // Core 层**自己的值表示** —— `Value::CoreClosure`（形状与
            // `Value::Closure` 同款：形参 / 捕获的环境 / 函数体；只是函数体是
            // Core IR 的 `CoreFunction`）。
            //
            // 捕获**整个环境**（与 MIR 层 `h_closure` 逐字同款）：闭包体里的
            // `EnvLoad` 要能读到外层绑定 ⇒ 捕获的环境必须是父链。
            //
            // ⚠ `captures` 字段（过近似的捕获名清单）**不参与**运行期 ——
            // 与 MIR 层同款（`h_closure` 也是捕获整个环境）。
            let closure = Value::CoreClosure {
                params: params.clone(),
                env: crate::value::EnvRef(Box::new(env.clone())),
                core_body: std::sync::Arc::new(body.clone()),
            };
            regs[*dst] = closure.clone();
            Ok(Some(closure))
        }
        CoreInst::ClosureCall(dst, callee, args) => {
            let callee_val = reg(regs, *callee)?;
            let arg_vals = arg_values(regs, args)?;
            let v = match &callee_val {
                // v0.104.6 D484：Core 闭包 → 执行 **Core 体**（不绕回 MIR 层）。
                Value::CoreClosure { .. } => {
                    call_core_closure(&callee_val, arg_vals, interp, effects)?
                }
                // MIR 闭包 / 内建可调用值 → 交给宿主（MIR 侧语义）。
                _ => interp.call_value(&callee_val, arg_vals, effects)?,
            };
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
        // 分支 / 跳转的**指令形态**：控制流由**终结指令**决定，指令形态只是
        // 冗余副本（MIR 引擎里 `Flow::Jump(_)` 同样是 no-op）⇒ 忽略。
        // （`Return` 的指令形态此前也在此列 ⇒ D487 随死变体一并删除。）
        CoreInst::Branch { .. } | CoreInst::Jump(_) => Ok(None),
        // ── v1 不支持的部分：响亮报错，不静默降级 ──
        // ── v0.104.6 D485：区域构造 ──
        //
        // 语义**逐条对齐** MIR 侧对应的 handler（`h_transaction` / `h_worker` /
        // `h_parallel` / `h_observe` / `h_span` / `h_with_config`）。
        CoreInst::Scope {
            kind,
            body,
            compensation,
            payload,
        } => {
            match kind.as_str() {
                // ── transaction：体失败 ⇒ 跑补偿 ⇒ 报「已回滚」──
                "transaction" => {
                    let comp = compensation
                        .as_ref()
                        .ok_or_else(|| format!("Core: transaction 区域缺补偿体（kind={kind}）"))?;
                    // 体在**隔离**环境里跑（与 MIR 侧 `run_isolated` 同款：
                    // 环境变更不外泄），效应仍向上传播。
                    let mut inner_env = env.clone();
                    match execute(body, &mut inner_env, interp, effects) {
                        Ok(_) => Ok(None),
                        Err(_) => {
                            let mut comp_env = env.clone();
                            if let Err(e) = execute(comp, &mut comp_env, interp, effects) {
                                eprintln!("[warn] transaction compensation failed: {}", e);
                            }
                            Err("Transaction rolled back".to_string())
                        }
                    }
                }
                // ── worker：隔离执行 ──
                //
                // MIR 侧 `h_worker` → `run_isolated`（克隆 env 跑完丢弃）⇒
                // worker 内的环境变更**不**外泄。
                "worker" => {
                    let mut inner_env = env.clone();
                    execute(body, &mut inner_env, interp, effects)?;
                    Ok(None)
                }
                // ── parallel：体在**当前**环境里跑 ──
                //
                // MIR 侧 `h_parallel` 把体里连续的 worker 声明**并发**跑，
                // 但明确保证「环境变更按**声明顺序**合并 ⇒ 并发完成顺序不影响
                // 最终状态」⇒ 顺序执行与并发执行**结果相同** ⇒ Core 侧顺序
                // 执行是忠实的。
                "parallel" => {
                    execute(body, env, interp, effects)?;
                    Ok(None)
                }
                // ── export：体在当前环境里跑，之后标记导出名 ──
                //
                // 与 MIR 侧同序（`fcfg_lower`：先降声明体，再发 `ExportMark`）。
                "export" => {
                    execute(body, env, interp, effects)?;
                    if let ScopePayload::Export(names) = payload {
                        for n in names {
                            env.exports.insert(n.clone());
                        }
                    }
                    Ok(None)
                }
                // ── with_config：设宿主 config → 跑体 → 恢复 ──
                //
                // 与 MIR 侧 `h_with_config` 同款：config 是**宿主状态**
                // （`MirHost::mir_with_config` / `mir_restore_config`）。
                "with_config" => {
                    let ScopePayload::WithConfig(bindings) = payload else {
                        return Err("Core: with_config 区域缺绑定载荷".to_string());
                    };
                    let mut vals: Vec<(String, Value)> = Vec::with_capacity(bindings.len());
                    for (name, r) in bindings {
                        vals.push((name.clone(), reg(regs, *r)?));
                    }
                    interp.mir_with_config(&vals)?;
                    let result = execute(body, env, interp, effects);
                    // 无论成败都恢复（与 MIR 侧同款）。
                    interp.mir_restore_config();
                    result?;
                    Ok(None)
                }
                // ── span / observe：记录 trace span → 隔离跑体 ──
                //
                // 与 MIR 侧 `h_span` / `h_observe` 同款：无 trace 能力的宿主
                // 退化为**仅执行体**（`trace_collector()` 返回 `None`）。
                "span" | "observe" => {
                    let ScopePayload::Trace { name, tags } = payload else {
                        return Err(format!("Core: {kind} 区域缺 trace 载荷"));
                    };
                    let attrs: std::collections::HashMap<String, String> =
                        tags.iter().cloned().collect();
                    let handle = interp
                        .trace_collector()
                        .map(|t| t.start_span(name, attrs.clone()));
                    let mut inner_env = env.clone();
                    let result = execute(body, &mut inner_env, interp, effects);
                    if let Some(h) = handle {
                        h.end(attrs);
                    }
                    result?;
                    Ok(None)
                }
                // ── prompt_section / document_section：body 求值 ⇒ 建 section 值
                //    ⇒ 绑定到环境 ──
                //
                // 与 MIR 侧 `h_prompt_section` / `h_document_section` 同款：
                // body 在**克隆**的环境里跑（文本求值），值用**同一份** helper
                // 建（`prompt_section_value` / `document_section_value`）⇒
                // 两个执行器建出的值**逐字相同**。
                "prompt_section" | "document_section" => {
                    let ScopePayload::Section { name } = payload else {
                        return Err(format!("Core: {kind} 区域缺 section 名字载荷"));
                    };
                    let mut child_env = env.clone();
                    let text = execute(body, &mut child_env, interp, effects)?;
                    let value = if kind == "prompt_section" {
                        crate::mir::handlers::prompt_section_value(name, text)
                    } else {
                        crate::mir::handlers::document_section_value(name, text)
                    };
                    env.define(name.clone(), value, false);
                    Ok(None)
                }
                // ── solve：建 goal ⇒ 关系搜索 ⇒ 解列表写回外层寄存器 ──
                //
                // 与 MIR 侧 `h_solve` 同款：查询变量按序分配 `Value::LogicVar`
                // 注入**新作用域层**（不外泄）⇒ 跑 goal 构建体（产物必须是
                // `Value::Goal`）⇒ 搜索（**值层面**，与 IR 无关 ⇒ 复用
                // `solve_from_goal`）⇒ 解列表写 `dst`。
                "solve" => {
                    let ScopePayload::Solve {
                        dst,
                        limit,
                        query_vars,
                        anon_vars,
                    } = payload
                    else {
                        return Err("Core: solve 区域缺查询参数载荷".to_string());
                    };
                    let mut builder_env = env.clone();
                    for (i, name) in query_vars.iter().enumerate() {
                        builder_env.define(name.clone(), Value::LogicVar(i as u64), false);
                    }
                    let base = query_vars.len() as u64;
                    for (i, name) in anon_vars.iter().enumerate() {
                        builder_env.define(name.clone(), Value::LogicVar(base + i as u64), false);
                    }
                    let built = execute(body, &mut builder_env, interp, effects)?;
                    let built_goal = match built {
                        Value::Goal(g) => *g,
                        other => {
                            return Err(format!(
                                "solve 的目标构建体必须返回 goal 值，得到 {}",
                                crate::flow::type_name(&other)
                            ));
                        }
                    };
                    let solutions = crate::mir::handlers::solve_from_goal(
                        built_goal,
                        *limit,
                        query_vars,
                        anon_vars.len() as u64,
                        env,
                        interp,
                        effects,
                    )?;
                    let value = Value::List(solutions.into());
                    regs[*dst] = value.clone();
                    // ⚠ solve **有值**（解列表）—— 与 MIR 侧 `MirInst::Solve` 的
                    // `dst()` = Some(dst) 同款 ⇒ 必须更新末值，否则程序的返回值
                    // 是 Nil（实测：`solve { edge(?x, ?y) }` 两边值不同）。
                    Ok(Some(value))
                }
                other => Err(format!("Core 执行器遇到未知区域构造 `{other}`")),
            }
        }
        // ── v0.104.6 D485：代数效果 ──
        //
        // 与 MIR 侧 `h_handle` / `h_perform` 同款：install 换下旧 handler、
        // perform 走**宿主**的 handler 栈、restore 把旧 handler 放回去。
        CoreInst::EffectInstall { label, handler } => {
            let name = label.name().to_string();
            let h = reg(regs, *handler)?;
            let Value::CoreClosure {
                core_body,
                env: captured,
                ..
            } = &h
            else {
                return Err(format!(
                    "Core: EffectInstall 的 handler 不是 Core 闭包（实际 {}）",
                    crate::flow::type_name(&h)
                ));
            };
            // 换下旧 handler（嵌套 handle 栈）—— 与 `h_handle` 的 take 同款。
            let prev = interp.take_effect_handler(&name);
            saved_handlers.push((name.clone(), prev));
            interp.install_effect_handler(
                name.clone(),
                Box::new(CoreHandlerClosure {
                    handler_core: core_body.clone(),
                    // handler 自有的执行环境（安装时的快照）—— 与 MIR 侧
                    // `HandlerClosure.env` 同款。
                    env: captured.0.as_ref().clone(),
                }),
            );
            Ok(None)
        }
        CoreInst::EffectPerform { dst, label, args } => {
            let name = label.name().to_string();
            let arg_vals = arg_values(regs, args)?;
            // 与 MIR 侧 `h_perform` 同款：走宿主的 handler 栈。
            match interp.perform_effect(&name, arg_vals, effects) {
                Some(reply) => {
                    regs[*dst] = reply.clone();
                    Ok(Some(reply))
                }
                None => Err(format!(
                    "unhandled effect: {name} (no matching handle block in scope)"
                )),
            }
        }
        CoreInst::EffectRestore { label } => {
            let name = label.name().to_string();
            // 弹出保存的旧 handler 并放回（与 `h_handle` 的 restore 同款）。
            let idx = saved_handlers.iter().rposition(|(n, _)| *n == name);
            let prev = match idx {
                Some(i) => saved_handlers.remove(i).1,
                None => None,
            };
            interp.restore_effect_handler(name, prev);
            Ok(None)
        }
        // ── v0.104.6 D485：编排执行 ──
        //
        // 载荷是 **witness 形态**（D483 原样透传）⇒ 执行前转成 MIR 形态
        // （`MirOrchestrateKind::from_witness_kind`，与 FCFG→MIR 桥接层
        // 同一机制），再交给 MIR 侧的 `h_orchestrate`（agent 图 / BSP 超步
        // 的语义在那一层）。
        CoreInst::Orchestrate {
            input_var,
            result_var,
            kind,
        } => {
            let mir_kind = crate::mir::orchestrate::MirOrchestrateKind::from_witness_kind(kind);
            crate::mir::handlers::h_orchestrate(
                interp, env, input_var, result_var, &mir_kind, effects,
            )?;
            Ok(None)
        }
        // ── v0.104.6 D486：关系定义 → 注册到环境 ──
        //
        // 与 MIR 侧 `h_rel_def` **同一份**（同名定义累积子句 = Prolog consult
        // 语义）⇒ 两个执行器注册出的关系**逐字相同**。
        CoreInst::RelDef { name, clauses } => {
            crate::mir::handlers::h_rel_def(env, name, clauses);
            Ok(None)
        }
        // ── v0.104.6 D488：声明型 → 在环境里注册绑定 ──
        //
        // 与 MIR 侧**同一份** handler（`h_type_alias` / `h_enum_def` /
        // `h_struct_def` / `h_model_def` / `h_msg_def`）⇒ 两个执行器注册出的
        // 绑定**逐字相同**。
        CoreInst::TypeAlias { name, target } => {
            crate::mir::handlers::h_type_alias(env, name, target);
            Ok(None)
        }
        CoreInst::EnumDef { name, variants } => {
            crate::mir::handlers::h_enum_def(env, name, variants);
            Ok(None)
        }
        CoreInst::StructDef { name, fields } => {
            crate::mir::handlers::h_struct_def(env, name, fields);
            Ok(None)
        }
        CoreInst::ModelDef { name, fields } => {
            crate::mir::handlers::h_model_def(env, name, fields);
            Ok(None)
        }
        CoreInst::MsgDef { name, variants } => {
            crate::mir::handlers::h_msg_def(env, name, variants);
            Ok(None)
        } // ── v0.104.6 D487：**没有「不支持」的分支了** ──
          //
          // 此前这里有 4 组报错分支（thunk / ADT 构造 / 字段访问 / phi）⇒
          // 「Core 执行器支持 Core 模型的**每一个**变体」这句话**不成立**。
          //
          // D487 查明：那些变体**全是零生产者的死变体**（语言层没有对应语法、
          // MIR 侧也没有对应指令）⇒ 已从模型里**删除** ⇒ 现在这个 match
          // **穷尽且无报错** ⇒ **编译器**保证「执行器覆盖模型的每个变体」。
          //
          // 守卫：`tests/core_no_dead_variants.rs`（零死变体普查）。
    }
}

/// **Core 闭包**装进宿主的代数效果 handler 槽。
///
/// v0.104.6 D485：MIR 侧的对应物是 `runtime::effect::HandlerClosure`（装
/// `MirFunction` handler 体）。Core 侧的 handler 体是 **Core IR** ⇒ 装不下 ⇒
/// 另配一个。
///
/// 与 MIR 侧**同款**的契约：
/// - `perform` 时把实参按 `__arg0` / `__arg1` … 注入 **handler 自有的环境**；
/// - handler 末尾表达式的值就是本次 perform 的**回复值**（single-shot：
///   handler 一次性消耗，不还原）。
struct CoreHandlerClosure {
    /// handler 体（Core IR 的子函数）。
    handler_core: std::sync::Arc<CoreFunction>,
    /// handler 自有的执行环境（安装时的快照）—— 跨多次 perform 保持 handler
    /// 侧写可见（与 `HandlerClosure.env` 同款）。
    env: Environment,
}

impl crate::runtime::effect::EffectHandler for CoreHandlerClosure {
    fn perform_box(
        &mut self,
        host: &mut dyn MirHost,
        args: Vec<Value>,
        effects: &mut Effects,
    ) -> Result<Value, String> {
        // 1. 实参注入 handler 环境（`__arg0`、`__arg1` …）。
        for (i, arg) in args.iter().enumerate() {
            self.env.define(format!("__arg{}", i), arg.clone(), false);
        }
        // 2. 跑 handler 体（Core IR）—— 与 MIR 侧 `HandlerClosure::perform_box`
        //    跑 `handler_mir` 同款，只是执行器不同。
        execute(&self.handler_core, &mut self.env, host, effects)
    }
}

/// 调用一个 **Core 闭包**（`Value::CoreClosure`）—— **唯一机制**。
///
/// 两个入口都走这里 ⇒ 闭包调用语义**逐字同一套**：
///
/// ① Core 执行器的**值调用**（`CoreInst::ClosureCall`）；
/// ② MIR 解释器的**具名调用**（名字在环境里绑定到 Core 闭包时）—— 那是
///    `h_call` 的「**环境**查找」分支的产物，只能由宿主发起，故委派回来。
///
/// arity 检查 / 子环境构造 / 按位置绑定，与 MIR 侧
/// `interpreter::dispatch::call_value_impl` 的 `Value::Closure` 分支**同款**。
pub fn call_core_closure(
    closure: &Value,
    args: Vec<Value>,
    host: &mut dyn MirHost,
    effects: &mut Effects,
) -> Result<Value, String> {
    let Value::CoreClosure {
        core_body,
        params,
        env: captured,
    } = closure
    else {
        return Err(format!("not a Core closure: {}", closure));
    };
    if args.len() != params.len() {
        return Err(format!(
            "closure expects {} args, got {}",
            params.len(),
            args.len()
        ));
    }
    // 子环境：捕获的环境作**父**，形参按位置绑进子层（与 MIR 侧同款）。
    let mut child_env =
        Environment::with_parent_of(std::sync::Arc::new(captured.0.as_ref().clone()));
    for (i, param) in params.iter().enumerate() {
        let val = args.get(i).cloned().unwrap_or(Value::Nil);
        child_env.define(param.clone(), val, false);
    }
    execute(core_body, &mut child_env, host, effects)
}

/// 实参值列表。
fn arg_values(regs: &[Value], args: &[usize]) -> Result<Vec<Value>, String> {
    args.iter().map(|r| reg(regs, *r)).collect()
}

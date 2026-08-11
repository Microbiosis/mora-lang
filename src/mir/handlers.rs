//! v0.59: MirInst handler functions — one per variant.
//!
//! Extracted from the monolithic `run_mir` match loop so that both the
//! linear interpreter (`run_mir`) and the DAG interpreter (`run_dag`)
//! share the same instruction-level logic.
//!
//! Each handler follows a consistent pattern:
//! - Pure value ops: write to `regs[dst]`, return `Flow::Continue`
//! - Side effects: modify `env`/`interp`, return `Flow::Continue` or error
//! - Control flow: return `Flow::Jump(label)` or `Flow::Return(value)`

use std::collections::HashMap;

use std::sync::Arc;

use super::vm::index_assign_value;

use crate::flow::is_truthy;

use super::vm::value_to_string;

use crate::common::BinaryOp;

use crate::flow::eval_binary;

use crate::mir::expr::{MirOrchestrateKind, MirPregelConfig};

use crate::mir::host::MirHost;

use crate::mir::vm::{index_value, run_mir, self_match_pattern};

use crate::mir::{MirFunction, Reg};

use crate::runtime::types::{TraitInfo, TraitMethodSig};

use crate::value::{Environment, Value};

/// What the linear interpreter should do after a handler runs.
#[derive(Debug)]
pub enum Flow {
    /// Advance pc by 1 (normal).
    Continue,
    /// Jump to the given label.
    Jump(usize),
    /// Return from the function with the given value.
    Return(Value),
    /// v0.70: Vote to halt. In Pregel context, the current agent signals
    /// "I'm done — don't reschedule me unless someone sends me a message."
    /// In linear context, behaves like Return.
    Halt(Option<Value>),
}

pub type HandlerResult = Result<Flow, String>;

// ============================================================
// Pure value instructions (write to regs[dst])
// ============================================================

pub fn h_const(regs: &mut [Value], dst: Reg, value: &Value) {
    regs[dst] = value.clone();
}

pub fn h_var(regs: &mut [Value], dst: Reg, name: &str, env: &Environment) {
    regs[dst] = env.get(name).unwrap_or(Value::Nil);
}

pub fn h_binary_op(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    op: &BinaryOp,
    rhs: Reg,
) -> Result<(), String> {
    let lv = regs[lhs].clone();
    let rv = regs[rhs].clone();
    regs[dst] = eval_binary(lv, op, rv)?;
    Ok(())
}

pub fn h_call(
    regs: &mut [Value],
    dst: Reg,
    name: &str,
    args: &[Reg],
    task_registry: &HashMap<&str, (&[String], &MirFunction)>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(), String> {
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    let result = if let Some((params, body)) = task_registry.get(name) {
        let mut child_env = env.clone();
        for (i, param) in params.iter().enumerate() {
            let val = arg_vals.get(i).cloned().unwrap_or(Value::Nil);
            child_env.define(param.clone(), val, false);
        }
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存（task body 借自指令表）
        run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
    } else if let Some(callable) = env.get(name) {
        // v0.75.76: 用户自定义 callable（Closure/Task/Compose/Partial）在
        // 执行 env 中直调（与 h_define 同一容器，无回落）；其余名（builtin、
        // 未定义等）统一经 mir_call_function —— 单一 env 传递，无回退分支。
        match callable {
            Value::Task { .. }
            | Value::Closure { .. }
            | Value::Compose(_)
            | Value::Partial(_, _) => interp.call_value(&callable, arg_vals)?,
            _ => interp.mir_call_function(name, arg_vals, env)?,
        }
    } else {
        interp.mir_call_function(name, arg_vals, env)?
    };
    regs[dst] = result;
    Ok(())
}

pub fn h_list_lit(regs: &mut [Value], dst: Reg, items: &[Reg]) {
    let vals: Vec<Value> = items.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = Value::List(vals);
}

pub fn h_dict_lit(regs: &mut [Value], dst: Reg, entries: &[(String, Reg)]) {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.clone(), regs[*v].clone());
    }
    regs[dst] = Value::Dict(map);
}

pub fn h_index(regs: &mut [Value], dst: Reg, obj: Reg, idx: Reg) -> Result<(), String> {
    let obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    regs[dst] = index_value(&obj_val, &idx_val)?;
    Ok(())
}

pub fn h_index_assign(regs: &mut [Value], obj: Reg, idx: Reg, val: Reg) -> Result<(), String> {
    let mut obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    let val_val = regs[val].clone();
    index_assign_value(&mut obj_val, &idx_val, &val_val)?;
    regs[obj] = obj_val;
    Ok(())
}

pub fn h_method_call(
    regs: &mut [Value],
    dst: Reg,
    receiver: Reg,
    method: &str,
    args: &[Reg],
    interp: &mut dyn MirHost,
) -> Result<(), String> {
    let recv_val = regs[receiver].clone();
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = interp.mir_call_method(recv_val, method, arg_vals)?;
    Ok(())
}

pub fn h_pipe(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    rhs: Reg,
    interp: &mut dyn MirHost,
) -> Result<(), String> {
    let lhs_val = regs[lhs].clone();
    let rhs_val = regs[rhs].clone();
    regs[dst] = interp.call_value(&rhs_val, vec![lhs_val])?;
    Ok(())
}

pub fn h_prompt(regs: &mut [Value], dst: Reg, parts: &[Reg]) {
    let mut s = String::new();
    for r in parts {
        s.push_str(&value_to_string(&regs[*r]));
    }
    regs[dst] = Value::String(s);
}

pub fn h_closure(
    regs: &mut [Value],
    dst: Reg,
    params: &[String],
    body: &MirFunction,
    env: &Environment,
) {
    // v0.75.77: 闭包捕获执行 env（与 h_define 写入同一容器，单一来源）——
    // 不再读 interp.environment() 宿主全局槽（take_env 移空后捕获到空壳，
    // 顶层绑定 base 对闭包不可见：`let base=10; let f=fn(x) x+base end`）。
    let closure = Value::Closure {
        params: params.to_vec(),
        env: crate::value::EnvRef(Box::new(env.clone())),
        mir_body: Arc::new(body.clone()),
    };
    regs[dst] = closure;
}

pub fn h_dyn_trait(
    regs: &mut [Value],
    dst: Reg,
    src: Reg,
    trait_name: &str,
    trait_generics: &[String],
) {
    let data = regs[src].clone();
    regs[dst] = Value::TraitObject {
        for_generics: Vec::new(),
        trait_generics: trait_generics.to_vec(),
        for_type: String::new(),
        trait_name: trait_name.to_string(),
        data: Box::new(data),
    };
}

// ============================================================
// Side-effect instructions (modify env / interp state)
// ============================================================

pub fn h_define(env: &mut Environment, name: &str, regs: &[Value], src: Reg) {
    env.define(name.to_string(), regs[src].clone(), false);
}

pub fn h_assign(env: &mut Environment, name: &str, regs: &[Value], src: Reg) {
    env.assign(name, regs[src].clone());
}

pub fn h_type_alias(env: &mut Environment, name: &str, target: &str) {
    env.define(name.to_string(), Value::String(target.to_string()), false);
}

pub fn h_enum_def(env: &mut Environment, name: &str, variants: &[crate::common::EnumVariant]) {
    let mut map = HashMap::new();
    for v in variants {
        map.insert(v.name.clone(), Value::String(v.name.clone()));
    }
    env.define(name.to_string(), Value::Dict(map), false);
}

pub fn h_struct_def(env: &mut Environment, name: &str, fields: &[crate::common::StructField]) {
    let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
    env.define(
        name.to_string(),
        Value::Dict(HashMap::from([(
            "__struct_fields__".to_string(),
            Value::List(
                field_names
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        )])),
        false,
    );
}

pub fn h_import(interp: &mut dyn MirHost, env: &mut Environment, path: &str) -> Result<(), String> {
    interp.mir_import(path, env)?;
    Ok(())
}

pub fn h_with_config(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    bindings: &[(String, Reg)],
    body: &MirFunction,
    jit: bool,
) -> Result<(), String> {
    let binding_vals: Vec<(String, Value)> = bindings
        .iter()
        .map(|(k, r)| (k.clone(), regs[*r].clone()))
        .collect();
    interp.mir_with_config(&binding_vals)?;
    let mut child_env = env.clone();

    let _result = if jit {
        // v0.75.43: copy-and-patch JIT（零 LLVM）— 直接编译 MirFunction，
        // 未覆盖指令回落解释器（run_jit Err → run_mir）。
        match crate::mir::jit::run_jit(body, interp, &mut child_env) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "JIT compilation failed ({}), falling back to MIR interpreter",
                    e
                );
                // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
            }
        }
    } else {
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存
        run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
    };
    interp.mir_restore_config();
    Ok(())
}

pub fn h_macro_def(env: &mut Environment, name: &str, params: &[String]) {
    env.define(
        name.to_string(),
        Value::Macro {
            name: name.to_string(),
            params: params.to_vec(),
        },
        false,
    );
}

/// v0.68: Unified isolated-block execution.
///
/// Clones `env`, runs `body` in the clone, then merges the child's changes
/// back into the parent using the interpreter's current merge strategies.
/// Returns the body's final value AND any merge conflicts (currently
/// discarded by callers; reserved for future observability hooks).
fn run_isolated(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(crate::value::Value, Vec<crate::value::Conflict>), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let result = run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?;
    let strategies = interp.current_merge_strategies();
    let conflicts = match strategies.as_ref() {
        Some(s) => env.merge_from_with_strategies(
            &child_env,
            s,
            &crate::value::MergeStrategy::LastWriteWins,
        ),
        None => {
            env.merge_from(&child_env, &crate::value::MergeStrategy::LastWriteWins);
            Vec::new()
        }
    };
    Ok((result, conflicts))
}

pub fn h_transaction(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    compensation: &MirFunction,
) -> Result<Flow, String> {
    match run_isolated(interp, env, body) {
        Ok(_) => Ok(Flow::Continue),
        Err(_) => {
            let mut comp_env = env.clone();
            // v0.75.9: 包裹 Arc 走全局 DAG 缓存
            if let Err(e) = run_mir(&Arc::new((*compensation).clone()), interp, &mut comp_env) {
                eprintln!("[warn] transaction compensation failed: {}", e);
            }
            Err("Transaction rolled back".to_string())
        }
    }
}

pub fn h_send(
    interp: &mut dyn MirHost,
    regs: &[Value],
    value: Reg,
    target: &str,
) -> Result<(), String> {
    let val = regs[value].clone();
    // v0.69: Push to dynamic_sends buffer. h_orchestrate flushes this into
    // the BSP engine's pending_sends before each super-step, so the message
    // reaches its target_node in the next super-step.
    // v0.70: Removed crossbeam worker_channels fallback (was dead code).
    interp.dynamic_sends().push(crate::checkpoint::SendTask {
        target_node: target.to_string(),
        input: val,
    });
    Ok(())
}

/// v0.71: Contribute a value to a per-super-step aggregator.
/// Currently a no-op when no Pregel run is active (aggregators are BSP-only).
pub fn h_aggregate(
    interp: &mut dyn MirHost,
    regs: &[Value],
    value: Reg,
    name: &str,
) -> Result<(), String> {
    // v0.75.83: 向 per-super-step 聚合器贡献值。agent 无法直接访问引擎，
    // 经 MirHost 缓冲提交（与 h_send → dynamic_sends 同构）；Pregel 引擎
    // 超步末 mem::take 收集并经 aggregator_contribute 归约，结果以
    // aggregator_<name> channel 暴露给下一超步。
    interp
        .aggregator_contributions()
        .push(crate::mir::expr::AggregatorContribution {
            name: name.to_string(),
            value: regs[value].clone(),
        });
    Ok(())
}

pub fn h_worker(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_observe(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    // v0.68: Bug fix — was discarding child_env mutations. Now merges
    // via run_isolated so observability side-effects (trace vars, span
    // markers) are actually visible.
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_span(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    // v0.68: Bug fix — same as h_observe.
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_save(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    value: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let value_str = value_to_string(&regs[value]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(value_str)],
        env,
    )?;
    Ok(())
}

pub fn h_load(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)], env)?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_read_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)], env)?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_write_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(content_str)],
        env,
    )?;
    Ok(())
}

pub fn h_append_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.append_text",
        vec![Value::String(path_str), Value::String(content_str)],
        env,
    )?;
    Ok(())
}

pub fn h_read_bytes_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let bytes = interp.mir_call_function("file.read_bytes", vec![Value::String(path_str)], env)?;
    env.define(var.to_string(), bytes, false);
    Ok(())
}

pub fn h_write_bytes_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_val = regs[content].clone();
    interp.mir_call_function(
        "file.write_bytes",
        vec![Value::String(path_str), content_val],
        env,
    )?;
    Ok(())
}

pub fn h_trait_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    name: &str,
    parents: &[String],
    methods: &[crate::mir::expr::MirTraitMethod],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    let sigs: Vec<TraitMethodSig> = methods
        .iter()
        .map(|m| TraitMethodSig {
            name: m.name.clone(),
            params: m
                .params
                .iter()
                .map(|p| (p.name.clone(), p.type_hint.as_ref().map(|t| t.name())))
                .collect(),
            return_type: m.return_type.clone(),
            has_self: m.params.first().map(|p| p.name == "self").unwrap_or(false),
        })
        .collect();
    Arc::make_mut(interp.trait_registry()).insert(
        name.to_string(),
        TraitInfo {
            name: name.to_string(),
            parents: parents.to_vec(),
            methods: sigs,
        },
    );
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = crate::runtime::types::default_impl_method_key(
                name,
                &Vec::<String>::new(),
                &m.name,
            );
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn h_impl_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    trait_name: &str,
    trait_generics: &[String],
    for_type: &str,
    for_generics: &[String],
    methods: &[crate::mir::expr::MirFnDef],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    Arc::make_mut(interp.impl_table())
        .entry(trait_name.to_string())
        .or_default()
        .push(for_type.to_string());
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = crate::runtime::types::impl_method_key(
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                &m.name,
            );
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}

pub fn h_orchestrate(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    input_var: &str,
    result_var: &str,
    kind: &MirOrchestrateKind,
) -> Result<(), String> {
    match kind {
        MirOrchestrateKind::Pregel {
            agents,
            edges,
            state_schema,
            checkpoint,
            interrupt_points,
            adjacency,
        } => {
            let config = MirPregelConfig {
                agents: agents.clone(),
                edges: edges.clone(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
                adjacency: adjacency.clone(),
                aggregators: Vec::new(),
                master_compute: None,
            };
            run_pregel_config(interp, env, config, input_var, result_var)
        }
        // v0.75.84: MoA（Mixture-of-Agents，arXiv:2406.04692）— 展开为
        // pregel 图：每层 L = [N 个 proposer 并行 ai.chat] → [聚合 agent
        // 综合]。proposer 结果经 aggregate（Concat）提交，聚合 agent 读
        // input_aggregator_layer_{L}_responses 综合；聚合结果写 result
        // channel，下一层 proposer 读上一层的 responses channel 继续。
        // 复用 v0.75.83 aggregate 缓冲通道 + pregel BSP，零新引擎机制。
        MirOrchestrateKind::Moa {
            layers,
            proposers,
            aggregator,
            prompt,
        } => {
            let config = build_moa_config(*layers, proposers, aggregator, prompt, input_var)?;
            run_pregel_config(interp, env, config, input_var, result_var)
        }
        // v0.75.85: MoE（Mixture-of-Experts，Shazeer 2017 稀疏门控）— 单轮
        // 线性流程：router 打分 → top-k 稀疏激活 → 专家执行 → 加权组合。
        // 顺序执行（MoE 无超步，不用 pregel 图）。
        MirOrchestrateKind::Moe {
            experts,
            router,
            top_k,
            prompt,
        } => run_moe(
            interp, env, experts, router, *top_k, prompt, input_var, result_var,
        ),
        MirOrchestrateKind::Sequential { agents } => {
            // v0.75.34: Sequential orchestrate 执行 — 按声明顺序逐个执行
            // agent 的 prelowered task_body，前一个 agent 的输出作为下一个
            // 的输入（pipeline），最终结果写入 result_var。
            // 输入注入沿用 pregel 契约：`input` 变量（input_var 的当前值）。
            let mut input_val = env.get(input_var).unwrap_or(Value::Nil);
            let mut result = Value::Nil;
            for agent in agents {
                if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                    return Err(format!(
                        "orchestrate: agent '{}' has empty task_body (lowering missing)",
                        agent.name
                    ));
                }
                // 每 agent 独立 env（克隆父级）：input 定义在私有副本上，
                // 避免跨 agent 污染；副作用写回见下方合并。
                let mut agent_env = env.clone();
                agent_env.define("input".to_string(), input_val.clone(), false);
                agent_env.clock.tick(&agent.name);
                result = crate::mir::vm::run_mir(
                    &std::sync::Arc::new(agent.task_body.clone()),
                    interp,
                    &mut agent_env,
                )?;
                // agent 期间 define 的变量合并回父 env（与 pregel 引擎
                // reconcile_outcome 的写回语义一致）。
                for (name, val) in agent_env.iter() {
                    if env.get(&name).is_none() {
                        env.define(name, val, false);
                    }
                }
                input_val = result.clone();
            }
            env.define(result_var.to_string(), result, false);
            Ok(())
        }
        other => Err(format!("orchestrate({:?}) not yet supported", other)),
    }
}

// ─── v0.75.85: MoE（Mixture-of-Experts）执行 ───────────────────────
// 稀疏门控（Shazeer 2017）：router 语言面 fn 打分 → top-k 稀疏激活 →
// 专家执行 → 加权组合。顺序单轮，无超步。
// 组合规则（引擎侧 Rust，Float 自由，不受语言数值塔约束）：
//   激活专家输出全为数值 → Σ(weightᵢ × outᵢ)，weightᵢ = scoreᵢ/top-k 分和
//   （归一化 softmax 权重）。
//   含 String（模型专家）→ top-1 选择（最高分专家输出）——加权求和无意义。

/// 执行 MoE：router 打分 → top-k → 专家执行 → 加权组合 → result_var 绑定。
#[allow(clippy::too_many_arguments)] // 与 h_eval 同型（orchestrate 执行签名簇）
fn run_moe(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    experts: &[crate::mir::expr::MirMoeExpert],
    router: &crate::mir::expr::MirExpr,
    top_k: usize,
    prompt: &crate::mir::expr::MirExpr,
    input_var: &str,
    result_var: &str,
) -> Result<(), String> {
    if experts.is_empty() {
        return Err("moe: experts must not be empty".to_string());
    }
    if top_k == 0 {
        return Err("moe: top_k must be >= 1".to_string());
    }

    let input_val = env.get(input_var).unwrap_or(Value::Nil);

    // 1. router 执行（语言面 fn）→ 分数 dict
    let router_val = eval_expr_value(interp, env, router)?;
    let scores = match interp.call_value(&router_val, vec![input_val.clone()])? {
        Value::Dict(d) => d,
        other => {
            return Err(format!(
                "moe: router must return a Dict of expert scores, got {:?}",
                other
            ));
        }
    };

    // 2. top-k 稀疏：按分数降序取前 top_k 个专家
    let mut scored: Vec<(String, f64)> = Vec::new();
    for (name, score) in &scores {
        let s = value_to_f64(score).ok_or_else(|| {
            format!(
                "moe: router score for '{}' must be a number, got {:?}",
                name, score
            )
        })?;
        scored.push((name.clone(), s));
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);
    if scored.is_empty() {
        return Err("moe: router returned no scores".to_string());
    }

    // 3. 激活专家执行
    let mut outputs: Vec<(String, f64, Value)> = Vec::new(); // (name, score, output)
    let mut score_sum = 0.0f64;
    for (name, score) in &scored {
        let expert = experts
            .iter()
            .find(|e| &e.name == name)
            .ok_or_else(|| format!("moe: router referenced unknown expert '{}'", name))?;
        let out = run_moe_expert(interp, env, expert, &input_val, prompt)?;
        score_sum += *score;
        outputs.push((name.clone(), *score, out));
    }

    // 4. 加权组合
    let result = combine_moe_outputs(&outputs, score_sum);
    env.define(result_var.to_string(), result, false);
    Ok(())
}

/// 执行单个专家：函数专家 call_value(fn, [input])；模型专家 ai.chat。
fn run_moe_expert(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    expert: &crate::mir::expr::MirMoeExpert,
    input_val: &Value,
    prompt: &crate::mir::expr::MirExpr,
) -> Result<Value, String> {
    let def_val = eval_expr_value(interp, env, &expert.def)?;
    match def_val {
        // 函数专家：Closure/Task/Compose/Partial → call_value
        Value::Closure { .. } | Value::Task { .. } | Value::Compose(_) | Value::Partial(_, _) => {
            interp.call_value(&def_val, vec![input_val.clone()])
        }
        // 模型专家：{model: "..."} → ai.chat(prompt, {model})
        Value::Dict(d) => {
            let model = match d.get("model") {
                Some(Value::String(m)) => m.clone(),
                _ => {
                    return Err(format!(
                        "moe: expert '{}' dict must have a 'model' string key",
                        expert.name
                    ));
                }
            };
            // prompt 表达式 → 值（含 {input} 插值，经 env 的 input 变量）
            let prompt_val = eval_expr_value(interp, env, prompt)?;
            let prompt_str = match prompt_val {
                Value::String(s) => s,
                other => other.to_string(),
            };
            // ai.chat 是方法调用（ai.chat(prompt, {model})），经 env 的
            // ai builtin + MethodCall 指令执行。
            let mut body: Vec<crate::mir::MirInst> = Vec::new();
            let mut nxt = 0usize;
            let ai_r = 0;
            body.push(crate::mir::MirInst::Var(ai_r, "ai".to_string()));
            nxt += 1;
            let prompt_r = nxt;
            body.push(crate::mir::MirInst::Const(
                prompt_r,
                Value::String(prompt_str),
            ));
            nxt += 1;
            let dict_r = nxt;
            let mut cfg = HashMap::new();
            cfg.insert("model".to_string(), Value::String(model));
            body.push(crate::mir::MirInst::Const(dict_r, Value::Dict(cfg)));
            nxt += 1;
            let res_r = nxt;
            body.push(crate::mir::MirInst::MethodCall(
                res_r,
                ai_r,
                "chat".to_string(),
                vec![prompt_r, dict_r],
            ));
            nxt += 1;
            let body_fn = MirFunction {
                params: vec![],
                body,
                n_regs: nxt,
                ..Default::default()
            };
            let mut expert_env = env.clone();
            crate::mir::vm::run_mir(&std::sync::Arc::new(body_fn), interp, &mut expert_env)
        }
        other => Err(format!(
            "moe: expert '{}' must be a function or {{model: \"...\"}} dict, got {:?}",
            expert.name, other
        )),
    }
}

/// 组合：全数值 → 归一化加权求和；含 String → top-1 选择。
fn combine_moe_outputs(outputs: &[(String, f64, Value)], score_sum: f64) -> Value {
    let all_numeric = outputs
        .iter()
        .all(|(_, _, o)| matches!(o, Value::Int(_) | Value::Float(_)));
    if all_numeric && score_sum > 0.0 {
        let mut acc = 0.0f64;
        for (_, score, out) in outputs {
            let v = match out {
                Value::Int(i) => *i as f64,
                Value::Float(f) => *f,
                _ => 0.0,
            };
            let w = score / score_sum;
            acc += w * v;
        }
        Value::Float(acc)
    } else {
        // 含 String（模型专家）→ top-1（输出已按分数降序）
        outputs
            .first()
            .map(|(_, _, o)| o.clone())
            .unwrap_or(Value::Nil)
    }
}

/// 执行 MirExpr → Value（lower 单表达式 + run_mir）。
/// 闭包构造经 h_closure（run_mir 内）产出 Value::Closure；dict 经 h_dict_lit。
fn eval_expr_value(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    expr: &crate::mir::expr::MirExpr,
) -> Result<Value, String> {
    let lowered = crate::mir::lower::lower_mir_exprs(std::slice::from_ref(expr))
        .map_err(|e| format!("moe: expert/router lowering failed: {}", e))?;
    let mut inner_env = env.clone();
    crate::mir::vm::run_mir(&std::sync::Arc::new(lowered), interp, &mut inner_env)
}

/// Value → f64（router 分数解析；Int/Float 均接受）。
fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// v0.75.84: pregel 图执行公共路径（Pregel / Moa 共用）。
/// 从 h_orchestrate Pregel 分支提取：checkpoint 恢复、input 通道初始化、
/// 冲突回调、dynamic_sends flush、run、result 绑定。
fn run_pregel_config(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    config: crate::mir::expr::MirPregelConfig,
    input_var: &str,
    result_var: &str,
) -> Result<(), String> {
    use crate::pregel::MirPregelEngine;
    let mut engine = MirPregelEngine::new(config);

    // v0.75.84: 注入执行环境（含 builtin ai 等）— pregel agent 的 env
    // 单一来源；不注入时回落 interpreter.environment()（单测路径）。
    // `__moa_input` 携带 input_var 原始值（agent env 的 `input` 是 pregel
    // delta JSON，MoA 首层 proposer 的 `{input}` 插值需要真值）。
    let mut base_env = env.clone();
    if let Some(v) = env.get(input_var) {
        base_env.define("__moa_input".to_string(), v, false);
    }
    engine = engine.with_base_env(std::sync::Arc::new(parking_lot::Mutex::new(base_env)));

    // v0.66: Wire PersistRuntime's checkpoint saver into the engine
    // so the auto-save block in BSP ADVANCE actually persists.
    if let Some(saver) = interp.checkpoint_saver() {
        engine = engine.with_checkpoint_saver(saver);
    }

    // v0.63: Resume from checkpoint if available
    let thread_id = "pregel"; // matches build_checkpoint default
    if let Ok(Some(cp)) = interp.load_checkpoint(thread_id) {
        engine.restore_checkpoint(&cp);
    }

    // Only init channels if starting fresh (not restored)
    if engine.current_step == 0 {
        let input_val = env.get(input_var).unwrap_or(Value::Nil);
        let mut initial = HashMap::new();
        initial.insert(input_var.to_string(), input_val);
        engine.init_channels(initial);
    }

    // v0.62: Collect conflicts via callback for exposure in result
    let captured: std::sync::Arc<parking_lot::Mutex<Vec<crate::value::Conflict>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let cb_captured = captured.clone();
    engine = engine.with_conflict_callback(std::sync::Arc::new(move |c| {
        cb_captured.lock().push(c.clone());
        true // always continue
    }));

    // v0.69: Flush dynamic_sends into engine before run
    let pending = std::mem::take(interp.dynamic_sends());
    engine.flush_pending_sends(pending);
    let result = engine.run(interp)?;

    // v0.62: Expose conflicts as a structured list
    let conflict_list: Vec<Value> = captured
        .lock()
        .iter()
        .map(|c| {
            let mut d: HashMap<String, Value> = HashMap::new();
            d.insert("key".into(), Value::String(c.key.clone()));
            d.insert("parent_value".into(), c.parent_value.clone());
            d.insert("child_value".into(), c.child_value.clone());
            Value::Dict(d)
        })
        .collect();
    env.define(
        format!("{}_conflicts", result_var),
        Value::List(conflict_list),
        false,
    );
    env.define(result_var.to_string(), result, false);
    Ok(())
}

// ─── v0.75.84: MoA（Mixture-of-Agents）pregel 图展开 ─────────────────
// 每层 L（1..=layers）：
//   超步 1: N 个 proposer agent 并行 `ai.chat(prompt_L, {model})` →
//           aggregate layer_{L}_responses（Concat）→ result channel
//   超步 2: 聚合 agent 读 input_aggregator_layer_{L}_responses，
//           `ai.chat(Synthesize: {responses}, {model: aggregator})` → result
// 边：@start → p_{1}_*；p_{L}_i → agg_L；agg_L → p_{L+1}_*；agg_layers → @exit
// 末层聚合结果 = engine.run 返回的 result channel。
// task_body 指令经 Rust 侧构造（MirInst 序列，与 parser emit 语义一致）。

/// 构造 MoA 展开的 pregel 图配置。
fn build_moa_config(
    layers: usize,
    proposers: &[String],
    aggregator: &str,
    prompt: &crate::mir::expr::MirExpr,
    input_var: &str,
) -> Result<crate::mir::expr::MirPregelConfig, String> {
    use crate::mir::expr::{MirEdgeDef, MirPregelConfig};
    if proposers.is_empty() {
        return Err("moa: proposers list must not be empty".to_string());
    }
    if layers == 0 {
        return Err("moa: layers must be >= 1".to_string());
    }

    let mut agents = Vec::new();
    let mut edges = Vec::new();

    // 首层 proposer 接入 @start；层间经聚合 agent 传递。
    for l in 1..=layers {
        for (i, model) in proposers.iter().enumerate() {
            let pname = format!("p_{}_{}", l, i + 1);
            let body = build_proposer_body(l, i, model, prompt, input_var);
            agents.push(crate::mir::expr::MirAgentDef {
                name: pname.clone(),
                task_expr: prompt.clone(),
                verify_expr: None,
                with_config: None,
                task_body: body,
                combiner_body: None,
            });
            // 边：首层从 @start，其余层从前一层聚合 agent
            let from = if l == 1 {
                "@start".to_string()
            } else {
                format!("agg_{}", l - 1)
            };
            edges.push(MirEdgeDef {
                from,
                to: pname.clone(),
                condition_expr: None,
                condition_body: None,
            });
            // 每 proposer → 本层聚合 agent
            edges.push(MirEdgeDef {
                from: pname,
                to: format!("agg_{}", l),
                condition_expr: None,
                condition_body: None,
            });
        }
        // 聚合 agent：读 layer_{L}_response_*（proposer Define 合并进共享 env）
        let aname = format!("agg_{}", l);
        let body = build_aggregator_body(l, aggregator, proposers.len());
        agents.push(crate::mir::expr::MirAgentDef {
            name: aname.clone(),
            task_expr: prompt.clone(),
            verify_expr: None,
            with_config: None,
            task_body: body,
            combiner_body: None,
        });
        // 末层聚合 → @exit；其余层 → 下一层 proposer（上面边已建）
        if l == layers {
            edges.push(MirEdgeDef {
                from: aname,
                to: "@exit".to_string(),
                condition_expr: None,
                condition_body: None,
            });
        }
    }

    Ok(MirPregelConfig {
        agents,
        edges,
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: HashMap::new(),
        // v0.75.84: MoA 走共享 env 合并投递（reconcile 将 proposer Define 的
        // layer_*_response_* 合并回共享 env，聚合 agent 经 parent 链读取）—
        // 版本快照机制对首次执行不投递 delta 通道，aggregate 通道路径不可靠。
        aggregators: Vec::new(),
        master_compute: None,
    })
}

/// proposer task_body：
///   prompt → ai.chat(prompt, {model}) → Define(layer_{L}_response_{idx})
///   结果经 reconcile_outcome 合并回共享 env，聚合 agent 经 parent 链读取
///   （版本快照机制对首次执行不投递 delta 通道，共享 env 合并是可靠路径）。
fn build_proposer_body(
    layer: usize,
    proposer_idx: usize,
    model: &str,
    prompt: &crate::mir::expr::MirExpr,
    input_var: &str,
) -> MirFunction {
    let mut body: Vec<crate::mir::MirInst> = Vec::new();
    let mut nxt = 0usize;
    let alloc = |nxt: &mut usize| {
        let r = *nxt;
        *nxt += 1;
        r
    };

    // 所有层：先把 `input` 覆盖为 __moa_input（base_env 注入的 input_var
    // 原始值）—— pregel 把 `input` 注入为 delta JSON，用户 prompt 的
    // `{input}` 插值需要真值。
    let input_val_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(
        input_val_reg,
        "__moa_input".to_string(),
    ));
    body.push(crate::mir::MirInst::Define(
        "input".to_string(),
        input_val_reg,
    ));

    let prompt_reg = if layer == 1 {
        // L=1: 用户 prompt 表达式 lower 的指令序列放 body 开头（寄存器从 0）。
        let lowered = crate::mir::lower::lower_mir_exprs(std::slice::from_ref(prompt))
            .unwrap_or_else(|_| MirFunction {
                params: vec![],
                body: vec![],
                n_regs: 0,
                ..Default::default()
            });
        body.extend(lowered.body.iter().cloned());
        nxt = nxt.max(lowered.n_regs);
        lowered
            .body
            .last()
            .and_then(|i| i.dst())
            .unwrap_or_else(|| {
                let r = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
                r
            })
    } else {
        // L>1: 用户 prompt + 前层聚合结果（agg_{L-1} Define 的 agg_result_{L-1}）
        let lowered = crate::mir::lower::lower_mir_exprs(std::slice::from_ref(prompt))
            .unwrap_or_else(|_| MirFunction {
                params: vec![],
                body: vec![],
                n_regs: 0,
                ..Default::default()
            });
        body.extend(lowered.body.iter().cloned());
        nxt = nxt.max(lowered.n_regs);
        let user_prompt_reg = lowered
            .body
            .last()
            .and_then(|i| i.dst())
            .unwrap_or_else(|| {
                let r = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
                r
            });
        let sep = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(
            sep,
            Value::String("\n\nPrevious layer response: ".to_string()),
        ));
        let prev = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Var(
            prev,
            format!("agg_result_{}", layer - 1),
        ));
        let joined = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined,
            user_prompt_reg,
            crate::common::BinaryOp::Add,
            sep,
        ));
        let joined2 = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined2,
            joined,
            crate::common::BinaryOp::Add,
            prev,
        ));
        joined2
    };

    let ai_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(ai_reg, "ai".to_string()));

    let dict_reg = alloc(&mut nxt);
    let mut cfg = HashMap::new();
    cfg.insert("model".to_string(), Value::String(model.to_string()));
    body.push(crate::mir::MirInst::Const(dict_reg, Value::Dict(cfg)));

    let res_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::MethodCall(
        res_reg,
        ai_reg,
        "chat".to_string(),
        vec![prompt_reg, dict_reg],
    ));

    // v0.75.84: 结果 Define 到私有 env → reconcile 合并回共享 env
    //（聚合 agent 读取的唯一可靠路径；版本快照对首次执行不投递 delta）。
    // 1-based 命名（聚合侧按 "1. " 编号展示）。
    body.push(crate::mir::MirInst::Define(
        format!("layer_{}_response_{}", layer, proposer_idx + 1),
        res_reg,
    ));

    let _ = input_var;
    MirFunction {
        params: vec![],
        body,
        n_regs: nxt.max(1),
        ..Default::default()
    }
}

/// 聚合 agent task_body：
///   读 layer_{L}_response_{1..N}（proposer Define 合并进共享 env）→ 拼接 →
///   ai.chat("Synthesize...: " + responses, {model}) → Define(agg_result_L)
///   （末层聚合结果 = engine.run 返回的 result channel = agg_layers 的 result）。
fn build_aggregator_body(layer: usize, aggregator: &str, n_proposers: usize) -> MirFunction {
    let mut body: Vec<crate::mir::MirInst> = Vec::new();
    let mut nxt = 0usize;
    let alloc = |nxt: &mut usize| {
        let r = *nxt;
        *nxt += 1;
        r
    };

    // 拼接所有 proposer 响应："1. {r1}\n2. {r2}..."
    let mut responses_reg: Option<Reg> = None;
    for i in 0..n_proposers {
        let num = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(
            num,
            Value::String(format!("{}. ", i + 1)),
        ));
        let var = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Var(
            var,
            format!("layer_{}_response_{}", layer, i + 1),
        ));
        let joined = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined,
            num,
            crate::common::BinaryOp::Add,
            var,
        ));
        responses_reg = match responses_reg {
            None => Some(joined),
            Some(prev) => {
                let nl = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(
                    nl,
                    Value::String("\n".to_string()),
                ));
                let sep = alloc(&mut nxt);
                body.push(crate::mir::MirInst::BinaryOp(
                    sep,
                    prev,
                    crate::common::BinaryOp::Add,
                    nl,
                ));
                let acc = alloc(&mut nxt);
                body.push(crate::mir::MirInst::BinaryOp(
                    acc,
                    sep,
                    crate::common::BinaryOp::Add,
                    joined,
                ));
                Some(acc)
            }
        };
    }
    let responses_reg = responses_reg.unwrap_or_else(|| {
        let r = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
        r
    });

    let c1 = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Const(
        c1,
        Value::String("Synthesize these responses into a single high-quality answer: ".to_string()),
    ));
    let prompt_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::BinaryOp(
        prompt_reg,
        c1,
        crate::common::BinaryOp::Add,
        responses_reg,
    ));

    let ai_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(ai_reg, "ai".to_string()));

    let dict_reg = alloc(&mut nxt);
    let mut cfg = HashMap::new();
    cfg.insert("model".to_string(), Value::String(aggregator.to_string()));
    body.push(crate::mir::MirInst::Const(dict_reg, Value::Dict(cfg)));

    let res_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::MethodCall(
        res_reg,
        ai_reg,
        "chat".to_string(),
        vec![prompt_reg, dict_reg],
    ));

    // 聚合结果 Define → 共享 env（L>1 proposer 读取 agg_result_{L-1}；
    // 末层结果经 reconcile 写 result channel，engine.run 返回）。
    body.push(crate::mir::MirInst::Define(
        format!("agg_result_{}", layer),
        res_reg,
    ));

    MirFunction {
        params: vec![],
        body,
        n_regs: nxt.max(1),
        ..Default::default()
    }
}

pub fn h_eval(
    regs: &[Value],
    env: &mut Environment,
    name: &str,
    given_reg: Reg,
    expects: &[Reg],
    tolerance: &Option<f64>,
) -> Result<(), String> {
    let given_val = regs[given_reg].clone();
    env.define("given".to_string(), given_val.clone(), false);
    for &expect_reg in expects {
        let expect_val = regs[expect_reg].clone();
        let pass = if let Some(tol) = tolerance {
            match (&given_val, &expect_val) {
                (Value::Float(g), Value::Float(e)) => (g - e).abs() <= *tol,
                (Value::Int(g), Value::Int(e)) => (*g as f64 - *e as f64).abs() <= *tol,
                _ => given_val == expect_val,
            }
        } else {
            given_val == expect_val
        };
        if !pass {
            return Err(format!(
                "eval '{}': assertion failed: given {:?}, expected {:?}",
                name, given_val, expect_val
            ));
        }
    }
    eprintln!("eval '{}': PASSED", name);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn h_skill_def(
    env: &mut Environment,
    name: &str,
    description: &Option<String>,
    version: &Option<String>,
    requires: &[String],
    tasks: &[crate::mir::expr::MirSkillTask],
    task_bodies: &[MirFunction],
    verify: &Option<crate::mir::expr::MirSkillVerify>,
    verify_body: &Option<MirFunction>,
) {
    let mut meta = HashMap::new();
    meta.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(d) = description {
        meta.insert("description".to_string(), Value::String(d.clone()));
    }
    if let Some(v) = version {
        meta.insert("version".to_string(), Value::String(v.clone()));
    }
    meta.insert(
        "requires".to_string(),
        Value::List(requires.iter().map(|r| Value::String(r.clone())).collect()),
    );
    for (task, _body) in tasks.iter().zip(task_bodies.iter()) {
        if let Some(mfn) = &task.body {
            meta.insert(
                task.name.clone(),
                Value::Task {
                    name: task.name.clone(),
                    params: task.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
            );
        }
    }
    if let Some(v) = verify {
        let vp: Vec<String> = v.params.iter().map(|p| p.name.clone()).collect();
        let empty = MirFunction {
            params: vp.clone(),
            body: Vec::new(),
            n_regs: 0,
            ..Default::default()
        };
        let verify_mir = v.body.clone().unwrap_or(empty);
        let _ = verify_body;
        meta.insert(
            "verify".to_string(),
            Value::Task {
                name: "verify".to_string(),
                params: vp,
                mir_body: Arc::new(verify_mir),
            },
        );
    }
    env.define(name.to_string(), Value::Dict(meta), false);
}

pub fn h_prompt_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&Arc::new((*body).clone()), interp, &mut child_env);
    Ok(())
}

pub fn h_document_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&Arc::new((*body).clone()), interp, &mut child_env);
    Ok(())
}

pub fn h_match_expr(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &mut [Value],
    val: Reg,
    arms: &[(String, Option<Reg>, Box<MirFunction>, Reg)],
) -> Result<(), String> {
    let val_val = regs[val].clone();
    let mut matched = false;
    for (pat_str, cond_reg, arm_func, output_reg) in arms {
        if self_match_pattern(&val_val, pat_str, cond_reg.as_ref().map(|r| &regs[*r]), env) {
            // v0.75.9: 包裹 Arc 走全局 DAG 缓存（arm body 借自指令表）
            let result = run_mir(&Arc::new((**arm_func).clone()), interp, env)?;
            regs[*output_reg] = result;
            matched = true;
            break;
        }
    }
    if !matched && let Some((_pat, _cond, _func, output_reg)) = arms.first() {
        regs[*output_reg] = Value::Nil;
    }
    Ok(())
}

// v0.75.26: h_stream_for 已删（StreamFor 死原语移除，见 MirInst 定义注释）。

// ============================================================
// Control flow handlers
// ============================================================

pub fn h_jump(target: usize) -> Flow {
    Flow::Jump(target)
}

pub fn h_jump_if(regs: &[Value], cond: Reg, target: usize) -> Flow {
    if is_truthy(&regs[cond]) {
        Flow::Jump(target)
    } else {
        Flow::Continue
    }
}

pub fn h_jump_if_not(regs: &[Value], cond: Reg, target: usize) -> Flow {
    if !is_truthy(&regs[cond]) {
        Flow::Jump(target)
    } else {
        Flow::Continue
    }
}

pub fn h_return(regs: &[Value], value: Option<Reg>) -> Flow {
    Flow::Return(value.map_or(Value::Nil, |r| regs[r].clone()))
}

/// v0.70: Vote to halt (Pregel semantics). In a BSP context the engine
/// marks the current vertex as Halted; it won't be rescheduled unless a
/// Send arrives. In a linear context, equivalent to return.
pub fn h_halt(regs: &[Value], value: Option<Reg>) -> Flow {
    Flow::Halt(value.map(|r| regs[r].clone()))
}

pub fn h_break(target: usize) -> Flow {
    Flow::Jump(target)
}

pub fn h_continue(target: usize) -> Flow {
    Flow::Jump(target)
}

// v0.75.56: MirInst metadata (dst/input_regs/map_regs/is_effect) + dispatch
// 已拆至 inst.rs；经 `crate::mir::inst` 直接访问（mod.rs `pub use inst::*`）。
pub use crate::mir::inst::dispatch;

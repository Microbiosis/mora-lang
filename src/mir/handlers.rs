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

use super::vm::is_truthy;

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
    // Aggregator contribution requires direct engine access.
    // For now, the BSP engine exposes aggregator values as channels
    // (aggregator_<name>) at the end of each step. We just record the
    // contribution locally so it's available if a worker reads it back.
    let val = regs[value].clone();
    let _ = (interp, name, val);
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
    use crate::pregel::MirPregelEngine;
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
            let mut engine = MirPregelEngine::new(config);

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

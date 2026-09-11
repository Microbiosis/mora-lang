//! v0.91: LMIR → MIR 逆降维 — 9 层架构反向桥接。
//!
//! 将 LmirInst（内存布局感知）映射回 MirInst（值语义层）。
//!
//! v0.91 覆盖范围：
//! - 常量 re-box（ConstInt/Float/Bool/String）
//! - 控制流传导（Branch/Jump/Return/Unreachable）
//! - 值操作透传（Var/Copy/BinaryOp/Call/ListLit/DictLit/Index）
//! - 环境操作透传（EnvLoad/EnvStore/EnvMutate）
//! - 其他透传（Expr/IndexAssign/Assign/MacroDef）
//!
//! 未覆盖原语（Alloc/Load/Store/Gep/GC/FFI）→ 跳过。

use crate::mir::core::CoreFunction;
use crate::mir::core_to_cmir::core_to_cmir;
use crate::mir::cmir_to_lmir::cmir_to_lmir;
use crate::mir::lmir::LmirInst;
use crate::mir::{MirFunction, MirInst, Value};

/// 将 LmirInst 列表逆降维为 MirInst 列表。
pub fn lmir_to_mir(insts: &[LmirInst]) -> Vec<MirInst> {
    insts.iter().filter_map(lower_lmir_inst).collect()
}

/// 从 MIR 指令列表推断需要的寄存器数量。
fn compute_n_regs_from_mir(insts: &[MirInst]) -> usize {
    let mut max_reg = 0usize;
    for inst in insts {
        match inst {
            MirInst::Const(r, _) => max_reg = max_reg.max(*r),
            MirInst::Var(r, _) => max_reg = max_reg.max(*r),
            MirInst::Copy(d, s) => max_reg = max_reg.max(*d).max(*s),
            MirInst::BinaryOp(d, l, _, r) => max_reg = max_reg.max(*d).max(*l).max(*r),
            MirInst::Call(d, _, args) => max_reg = max_reg.max(*d).max(args.iter().fold(0, |m, r| m.max(*r))),
            MirInst::ListLit(r, items) => max_reg = max_reg.max(*r).max(items.iter().fold(0, |m, r| m.max(*r))),
            MirInst::DictLit(r, entries) => max_reg = max_reg.max(*r).max(entries.iter().fold(0, |m, (_, r)| m.max(*r))),
            MirInst::Index(d, o, i) => max_reg = max_reg.max(*d).max(*o).max(*i),
            MirInst::IndexAssign(o, i, v) => max_reg = max_reg.max(*o).max(*i).max(*v),
            MirInst::MethodCall(d, r, _, args) => max_reg = max_reg.max(*d).max(*r).max(args.iter().fold(0, |m, r| m.max(*r))),
            MirInst::TaskDef { body, .. } => {
                max_reg = max_reg.max(compute_n_regs_from_mir(&body.body));
            }
            MirInst::DynTrait { dst, src, .. } => max_reg = max_reg.max(*dst).max(*src),
            MirInst::Prompt(d, parts) => max_reg = max_reg.max(*d).max(parts.iter().fold(0, |m, r| m.max(*r))),
            MirInst::MatchExpr { val, arms } => {
                max_reg = max_reg.max(*val);
                for (_, _, _body, output_reg) in arms {
                    max_reg = max_reg.max(*output_reg);
                }
            }
            MirInst::Closure { dst, body: _, .. } => {
                max_reg = max_reg.max(*dst);
            }
            MirInst::Expr(r) => max_reg = max_reg.max(*r),
            MirInst::Return(r) => max_reg = max_reg.max(r.unwrap_or(0)),
            MirInst::Halt(r) => max_reg = max_reg.max(r.unwrap_or(0)),
            MirInst::Break(r) => max_reg = max_reg.max(*r),
            MirInst::Continue(r) => max_reg = max_reg.max(*r),
            MirInst::Assign(_, r) => max_reg = max_reg.max(*r),
            MirInst::Define(_, r) => max_reg = max_reg.max(*r),
            _ => {}
        }
    }
    max_reg + 1
}

/// 单条 LmirInst → Option<MirInst>。
fn lower_lmir_inst(inst: &LmirInst) -> Option<MirInst> {
    match inst {
        // ── 常量 re-box ──
        LmirInst::ConstInt(reg, n) => Some(MirInst::Const(*reg, Value::Int(*n))),
        LmirInst::ConstFloat(reg, f) => Some(MirInst::Const(*reg, Value::Float(*f))),
        LmirInst::ConstBool(reg, b) => Some(MirInst::Const(*reg, Value::Bool(*b))),
        LmirInst::ConstString(reg, _ptr, len) => {
            // LMIR 层字符串用 pointer+length 表示；逆降维时重建 Rust String。
            // 注意：这是 unsafe 的，仅用于骨架验证；生产路径应走 proper FFI。
            let s = unsafe {
                let slice = std::slice::from_raw_parts(*_ptr, *len);
                std::str::from_utf8_unchecked(slice)
            };
            Some(MirInst::Const(*reg, Value::String(s.to_string())))
        }
        LmirInst::ConstNil(reg) => Some(MirInst::Const(*reg, Value::Nil)),

        // ── 控制流（v0.91：Core 控制流传导，逆降维回 MIR） ──
        LmirInst::Branch {
            cond,
            true_bb: _true_bb,
            false_bb,
        } => Some(MirInst::JumpIfNot(*cond, *false_bb)), // MIR 用 JumpIfNot + post-patch
        LmirInst::Jump(target) => Some(MirInst::Jump(*target)),
        LmirInst::Return(value) => Some(MirInst::Return(*value)),
        LmirInst::Unreachable => Some(MirInst::Halt(None)),

        // ── 值操作透传（v0.91：Core→LMIR 语义保持） ──
        LmirInst::Var(reg, name) => Some(MirInst::Var(*reg, name.clone())),
        LmirInst::Copy(dst, src) => Some(MirInst::Copy(*dst, *src)),
        LmirInst::BinaryOp(dst, lhs, op, rhs) => {
            Some(MirInst::BinaryOp(*dst, *lhs, op.clone(), *rhs))
        }
        LmirInst::Call { dst, callee, args, name } => {
            let callee_name = name.clone().unwrap_or_else(|| format!("_r{}", callee));
            Some(MirInst::Call(*dst, callee_name, args.clone()))
        }
        LmirInst::MethodCall { dst, receiver, method, args } => {
            Some(MirInst::MethodCall(*dst, *receiver, method.clone(), args.clone()))
        }
        LmirInst::ListLit(reg, items) => Some(MirInst::ListLit(*reg, items.clone())),
        LmirInst::DictLit(reg, entries) => {
            Some(MirInst::DictLit(*reg, entries.clone()))
        }
        LmirInst::Index(dst, obj, idx) => Some(MirInst::Index(*dst, *obj, *idx)),

        // ── 环境操作透传 ──
        LmirInst::EnvLoad(reg, name) => Some(MirInst::Var(*reg, name.clone())),
        LmirInst::EnvStore(name, reg) => Some(MirInst::Define(name.clone(), *reg)),
        LmirInst::EnvMutate(name, reg) => Some(MirInst::Assign(name.clone(), *reg)),

        // ── 其他透传 ──
        LmirInst::Expr(reg) => Some(MirInst::Expr(*reg)),
        LmirInst::IndexAssign(obj, idx, value) => {
            Some(MirInst::IndexAssign(*obj, *idx, *value))
        }
        LmirInst::Assign(name, reg) => Some(MirInst::Assign(name.clone(), *reg)),
        LmirInst::MacroDef { name, params, body } => {
            let body_mir = lower_core_function_to_mir(body);
            let body_n_regs = compute_n_regs_from_mir(&body_mir);
            Some(MirInst::MacroDef {
                name: name.clone(),
                params: params.clone(),
                body: Box::new(MirFunction {
                    params: vec![],
                    body: body_mir,
                    n_regs: body_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
            })
        }

        LmirInst::Perform { dst, effect, args } => {
            Some(MirInst::Perform { dst: *dst, effect: effect.clone(), args: args.clone() })
        }
        LmirInst::Handle { effect, body, handler, k_param, k_dst } => {
            let body_mir = lower_core_function_to_mir(body);
            let handler_mir = lower_core_function_to_mir(handler);
            let body_n_regs = compute_n_regs_from_mir(&body_mir);
            let handler_n_regs = compute_n_regs_from_mir(&handler_mir);
            Some(MirInst::Handle {
                effect: effect.clone(),
                body: Box::new(MirFunction {
                    params: vec![],
                    body: body_mir,
                    n_regs: body_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
                handler: Box::new(MirFunction {
                    params: vec![],
                    body: handler_mir,
                    n_regs: handler_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
                k_param: k_param.clone(),
                k_dst: *k_dst,
            })
        }
        LmirInst::MatchExpr { scrutinee, arms } => {
            let mut mir_arms = Vec::new();
            for (variant, guard, body, output_reg) in arms {
                let body_mir = lower_core_function_to_mir(body);
                let body_n_regs = compute_n_regs_from_mir(&body_mir);
                mir_arms.push((
                    variant.clone(),
                    *guard,
                    Box::new(MirFunction {
                        params: vec![],
                        body: body_mir,
                        n_regs: body_n_regs,
                        effects: crate::mir::effect::EffectRow::Empty,
                    }),
                    *output_reg,
                ));
            }
            Some(MirInst::MatchExpr {
                val: *scrutinee,
                arms: mir_arms,
            })
        }

        LmirInst::Closure { dst, params, body } => {
            let body_mir = lower_core_function_to_mir(body);
            let body_n_regs = compute_n_regs_from_mir(&body_mir);
            Some(MirInst::Closure {
                dst: *dst,
                params: params.clone(),
                body: Box::new(MirFunction {
                    params: vec![],
                    body: body_mir,
                    n_regs: body_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
            })
        }
        LmirInst::ClosureCall { dst, callee, args } => {
            let callee_name = format!("_r{}", callee);
            Some(MirInst::Call(*dst, callee_name, args.clone()))
        }
        LmirInst::TaskDef { name, params, body } => {
            let body_mir = lower_core_function_to_mir(body);
            let body_n_regs = compute_n_regs_from_mir(&body_mir);
            Some(MirInst::TaskDef {
                name: name.clone(),
                params: params.clone(),
                body: Box::new(MirFunction {
                    params: vec![],
                    body: body_mir,
                    n_regs: body_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
            })
        }
        LmirInst::ModelDef { name, fields } => {
            Some(MirInst::ModelDef {
                name: name.clone(),
                fields: fields.clone(),
            })
        }
        LmirInst::MsgDef { name, variants } => {
            Some(MirInst::MsgDef {
                name: name.clone(),
                variants: variants.clone(),
            })
        }
        LmirInst::UpdateDef { name, params, body } => {
            let body_mir = lower_core_function_to_mir(body);
            let body_n_regs = compute_n_regs_from_mir(&body_mir);
            Some(MirInst::UpdateDef {
                name: name.clone(),
                params: params.clone(),
                body: Box::new(MirFunction {
                    params: vec![],
                    body: body_mir,
                    n_regs: body_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
            })
        }
        LmirInst::AppDef { name, model_name, msg_name, init, update, view } => {
            let init_mir = lower_core_function_to_mir(init);
            let update_mir = lower_core_function_to_mir(update);
            let view_mir = lower_core_function_to_mir(view);
            let init_n_regs = compute_n_regs_from_mir(&init_mir);
            let update_n_regs = compute_n_regs_from_mir(&update_mir);
            let view_n_regs = compute_n_regs_from_mir(&view_mir);
            Some(MirInst::AppDef {
                name: name.clone(),
                model_name: model_name.clone(),
                msg_name: msg_name.clone(),
                init_mir: Box::new(MirFunction {
                    params: vec![],
                    body: init_mir,
                    n_regs: init_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
                update_mir: Box::new(MirFunction {
                    params: vec![],
                    body: update_mir,
                    n_regs: update_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
                view_mir: Box::new(MirFunction {
                    params: vec![],
                    body: view_mir,
                    n_regs: view_n_regs,
                    effects: crate::mir::effect::EffectRow::Empty,
                }),
            })
        }

        // ── 内存/GC/FFI 原语：当前 MIR 层无对应指令，跳过 ──
        LmirInst::Alloc { .. }
        | LmirInst::Load { .. }
        | LmirInst::Store { .. }
        | LmirInst::Gep { .. }
        | LmirInst::GcAlloc { .. }
        | LmirInst::GcRoot(_)
        | LmirInst::GcBarrier(_)
        | LmirInst::RefCount { .. }
        | LmirInst::ExternCall { .. }
        | LmirInst::ExternTypeCast { .. }
        | LmirInst::BlockStart(_)
        | LmirInst::BlockEnd => None,
    }
}

/// 将 CoreFunction 通过完整 9 层管线逆降维为 MirInst 列表。
fn lower_core_function_to_mir(func: &CoreFunction) -> Vec<MirInst> {
    let cmir = core_to_cmir(func);
    let (lmir_insts, _) = cmir_to_lmir(&cmir);
    let mut mir = lmir_to_mir(&lmir_insts);
    // 对齐 lower_fcfg 约定：嵌套函数体末尾不保留冗余控制流 terminator。
    while let Some(MirInst::Return(_) | MirInst::Jump(_) | MirInst::JumpIf(_, _) | MirInst::JumpIfNot(_, _)) =
        mir.last()
    {
        mir.pop();
    }
    mir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;

    #[test]
    fn const_int_roundtrip() {
        let lmir = vec![LmirInst::ConstInt(0, 42)];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 1);
        assert!(matches!(&mir[0], MirInst::Const(0, Value::Int(42))));
    }

    #[test]
    fn const_float_roundtrip() {
        let lmir = vec![LmirInst::ConstFloat(1, std::f64::consts::PI)];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 1);
        assert!(matches!(&mir[0], MirInst::Const(1, Value::Float(f)) if (*f - std::f64::consts::PI).abs() < 1e-10));
    }

    #[test]
    fn const_bool_roundtrip() {
        let lmir = vec![LmirInst::ConstBool(2, true)];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 1);
        assert!(matches!(&mir[0], MirInst::Const(2, Value::Bool(true))));
    }

    #[test]
    fn control_flow_roundtrip() {
        let lmir = vec![
            LmirInst::Branch {
                cond: 1,
                true_bb: 10,
                false_bb: 20,
            },
            LmirInst::Jump(30),
            LmirInst::Return(Some(5)),
            LmirInst::Unreachable,
        ];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 4);
        assert!(matches!(&mir[0], MirInst::JumpIfNot(1, 20)));
        assert!(matches!(&mir[1], MirInst::Jump(30)));
        assert!(matches!(&mir[2], MirInst::Return(Some(5))));
        assert!(matches!(&mir[3], MirInst::Halt(None)));
    }

    #[test]
    fn value_ops_roundtrip() {
        let lmir = vec![
            LmirInst::Var(0, "x".into()),
            LmirInst::Copy(1, 0),
            LmirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            LmirInst::Call { dst: 3, callee: 4, args: vec![0, 1], name: None },
            LmirInst::Index(5, 0, 1),
        ];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 5);
        assert!(matches!(&mir[0], MirInst::Var(0, _)));
        assert!(matches!(&mir[1], MirInst::Copy(1, 0)));
        assert!(matches!(&mir[2], MirInst::BinaryOp(2, 0, _, 1)));
        assert!(matches!(&mir[3], MirInst::Call(3, _, _)));
        assert!(matches!(&mir[4], MirInst::Index(5, 0, 1)));
    }

    #[test]
    fn unsupported_lmir_inst_skipped() {
        use crate::mir::lmir::MemLayout;
        let lmir = vec![
            LmirInst::ConstInt(0, 1),
            LmirInst::Alloc { dst: 1, layout: MemLayout::int64() },
            LmirInst::ConstInt(2, 2),
        ];
        let mir = lmir_to_mir(&lmir);
        assert_eq!(mir.len(), 2);
        assert!(matches!(&mir[0], MirInst::Const(0, Value::Int(1))));
        assert!(matches!(&mir[1], MirInst::Const(2, Value::Int(2))));
    }
}

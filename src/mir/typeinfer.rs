//! α.8: SSA 类型推断
//!
//! 数据流分析，从 Const/Var 等源头传播类型到每个寄存器。
//! 为后续 SSA → LLVM IR → JIT 提供类型信息。
//!
//! 算法（单块内传播 + phi 合并）：
//! 1. 收集每个块的类型定义（Const/Var/BinaryOp 等）
//! 2. 迭代传播直到不动点
//! 3. Phi 节点取各 incoming 类型的合并（取最大类型）
//!
//! 约束：C2 手写 / I5 可回退

use crate::mir::ssa::{MirSsaFunction, RegType, SsaInst, SsaReg};
use crate::value::Value;

/// 对 SSA 函数执行类型推断
pub fn infer_types(ssa: &mut MirSsaFunction) {
    let n = ssa.blocks.len();
    if n == 0 {
        return;
    }

    // 找出最大寄存器号，分配类型数组
    let mut max_reg = 0usize;
    for (i, (_, ssa_reg)) in ssa.params.iter().enumerate() {
        if i > max_reg {
            max_reg = i;
        }
        if *ssa_reg > max_reg {
            max_reg = *ssa_reg;
        }
    }
    for block in &ssa.blocks {
        for inst in &block.insts {
            let dst = ssa_dst(inst);
            if dst > max_reg {
                max_reg = dst;
            }
            for src in sources(inst) {
                if src > max_reg {
                    max_reg = src;
                }
            }
        }
        for phi in &block.phis {
            if phi.dst > max_reg {
                max_reg = phi.dst;
            }
            for (_, src) in &phi.incoming {
                if *src > max_reg {
                    max_reg = *src;
                }
            }
        }
    }

    let mut types: Vec<RegType> = vec![RegType::Any; max_reg.saturating_add(1)];

    // 初始：参数类型为 Any（从外部传入，无法推断）
    for (_, ssa_reg) in &ssa.params {
        if *ssa_reg < types.len() {
            types[*ssa_reg] = RegType::Any;
        }
    }

    // 迭代传播直到不动点
    let mut changed = true;
    let mut iter = 0;
    while changed && iter < 50 {
        changed = false;
        iter += 1;

        // 第一遍：从所有块收集类型定义
        for block in &ssa.blocks {
            for inst in &block.insts {
                let dst = ssa_dst(inst);
                let inferred = infer_inst_type(inst, &types);
                if let Some(t) = inferred
                    && dst < types.len()
                {
                    let current = types[dst].clone();
                    if current != t {
                        types[dst] = merge_types(&current, &t);
                        changed = true;
                    }
                }
            }
        }

        // 第二遍：Phi 节点取各 incoming 类型的合并
        for block in &ssa.blocks {
            for phi in &block.phis {
                let mut merged = RegType::Any;
                for (_, src) in &phi.incoming {
                    if *src < types.len() {
                        let src_ty = types[*src].clone();
                        merged = merge_types(&merged, &src_ty);
                    }
                }
                if phi.dst < types.len() {
                    let current = types[phi.dst].clone();
                    if current != merged {
                        types[phi.dst] = merged;
                        changed = true;
                    }
                }
            }
        }
    }

    ssa.types = types;
}

fn ssa_dst(inst: &SsaInst) -> SsaReg {
    match inst {
        SsaInst::Const(d, _)
        | SsaInst::Var(d, _)
        | SsaInst::BinaryOp(d, _, _, _)
        | SsaInst::Call(d, _, _)
        | SsaInst::ListLit(d, _)
        | SsaInst::DictLit(d, _)
        | SsaInst::Index(d, _, _)
        | SsaInst::IndexAssign(d, _, _)
        | SsaInst::MethodCall(d, _, _, _)
        | SsaInst::Pipe(d, _, _)
        | SsaInst::Prompt(d, _)
        | SsaInst::Copy(d, _)
        | SsaInst::Define(_, d)
        | SsaInst::Assign(_, d)
        | SsaInst::Expr(d) => *d,
    }
}

fn sources(inst: &SsaInst) -> Vec<SsaReg> {
    let mut out = Vec::new();
    match inst {
        SsaInst::BinaryOp(_, l, _, r) => {
            out.push(*l);
            out.push(*r);
        }
        SsaInst::Call(_, _, args) => out.extend(args.iter().cloned()),
        SsaInst::ListLit(_, items) => out.extend(items.iter().cloned()),
        SsaInst::DictLit(_, pairs) => out.extend(pairs.iter().map(|(_, v)| *v)),
        SsaInst::Index(_, obj, idx) => {
            out.push(*obj);
            out.push(*idx);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            out.push(*obj);
            out.push(*idx);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            out.push(*recv);
            out.extend(args.iter().cloned());
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            out.push(*lhs);
            out.push(*rhs);
        }
        SsaInst::Prompt(_, parts) => out.extend(parts.iter().cloned()),
        SsaInst::Copy(_, src) => out.push(*src),
        SsaInst::Define(_, src) => out.push(*src),
        SsaInst::Assign(_, src) => out.push(*src),
        SsaInst::Expr(src) => out.push(*src),
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
    }
    out
}

fn infer_inst_type(inst: &SsaInst, types: &[RegType]) -> Option<RegType> {
    match inst {
        SsaInst::Const(_, value) => Some(value_type(value)),
        SsaInst::Var(_, _) => None, // 变量类型无法推断，保持 Any
        SsaInst::BinaryOp(_, l, _, r) => {
            let lt = if *l < types.len() {
                types[*l].clone()
            } else {
                RegType::Any
            };
            let rt = if *r < types.len() {
                types[*r].clone()
            } else {
                RegType::Any
            };
            Some(infer_binary_type(&lt, &rt))
        }
        SsaInst::Call(_, _, _) => None, // 调用类型需要类型签名表
        SsaInst::ListLit(_, items) => {
            // 取第一个元素的类型
            let mut elem_type = RegType::Any;
            for item in items {
                if *item < types.len() {
                    elem_type = types[*item].clone();
                    break;
                }
            }
            Some(RegType::List(Box::new(elem_type)))
        }
        SsaInst::DictLit(_, _) => Some(RegType::Dict(vec![RegType::Any, RegType::Any])),
        SsaInst::Index(_, obj, idx) => {
            // 从 list[T] → T，从 dict[K,V] → V
            let ot = if *obj < types.len() {
                types[*obj].clone()
            } else {
                RegType::Any
            };
            let it = if *idx < types.len() {
                types[*idx].clone()
            } else {
                RegType::Any
            };
            match (ot, it) {
                (RegType::List(elem), _) => Some(*elem),
                (RegType::Dict(_), RegType::String) => Some(RegType::Any),
                _ => Some(RegType::Any),
            }
        }
        SsaInst::IndexAssign(_, _, _) => Some(RegType::Void),
        SsaInst::MethodCall(_, _, _, _) => None,
        SsaInst::Pipe(_, _, _) => None,
        SsaInst::Prompt(_, _) => Some(RegType::String),
        SsaInst::Copy(_, src) => {
            if *src < types.len() {
                Some(types[*src].clone())
            } else {
                Some(RegType::Any)
            }
        }
        SsaInst::Define(_, src) => {
            if *src < types.len() {
                Some(types[*src].clone())
            } else {
                Some(RegType::Any)
            }
        }
        SsaInst::Assign(_, src) => {
            if *src < types.len() {
                Some(types[*src].clone())
            } else {
                Some(RegType::Any)
            }
        }
        SsaInst::Expr(src) => {
            if *src < types.len() {
                Some(types[*src].clone())
            } else {
                Some(RegType::Any)
            }
        }
    }
}

fn value_type(value: &Value) -> RegType {
    match value {
        Value::Int(_) => RegType::Int,
        Value::Float(_) => RegType::Float,
        Value::Bool(_) => RegType::Bool,
        Value::String(_) => RegType::String,
        Value::Nil => RegType::Void,
        Value::List(_) => RegType::List(Box::new(RegType::Any)),
        Value::Dict(_) => RegType::Dict(vec![RegType::String, RegType::Any]),
        _ => RegType::Any,
    }
}

fn infer_binary_type(lt: &RegType, rt: &RegType) -> RegType {
    // 数值运算：int + int = int, float + float = float, mixed = float
    if lt == &RegType::Float || rt == &RegType::Float {
        return RegType::Float;
    }
    if lt == &RegType::Int && rt == &RegType::Int {
        return RegType::Int;
    }
    if lt == &RegType::Bool || rt == &RegType::Bool {
        return RegType::Bool;
    }
    RegType::Any
}

fn merge_types(a: &RegType, b: &RegType) -> RegType {
    // 类型合并：取更具体的类型（不丢失信息）
    if a == b {
        return a.clone();
    }
    // 任何 Any 合并返回另一个
    if a == &RegType::Any {
        return b.clone();
    }
    if b == &RegType::Any {
        return a.clone();
    }
    // 数值类型合并：int + float → float
    if (a == &RegType::Int && b == &RegType::Float) || (a == &RegType::Float && b == &RegType::Int)
    {
        return RegType::Float;
    }
    // list<T> + list<U> → list<merge(T,U)>
    if let (RegType::List(at), RegType::List(bt)) = (a, b) {
        return RegType::List(Box::new(merge_types(at, bt)));
    }
    RegType::Any
}

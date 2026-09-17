//! Control flow handlers — Jump, Return, Break, Continue, Match, Quasiquote.

use crate::flow::is_truthy;

use crate::mir::vm::{run_mir, self_match_pattern, value_to_string};

use crate::mir::Reg;

use crate::mir::host::MirHost;

use crate::value::{Environment, Value};

// ============================================================
// Control flow handlers
// ============================================================

pub fn h_match_expr(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &mut [Value],
    val: Reg,
    arms: &[crate::mir::MatchArmInst],
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let val_val = regs[val].clone();
    let mut matched = false;
    for (pat_str, guard_func, arm_func, output_reg) in arms {
        if !self_match_pattern(&val_val, pat_str, None, env) {
            continue;
        }
        // v0.104.3: 守卫在**模式绑定之后**求值 —— `self_match_pattern` 已把
        // 绑定变量 define 进 `env`，守卫体因此能读到它们；再按真值决定是否
        // 采用该 arm。此前守卫是「外层寄存器」，而绑定只在匹配时才存在 →
        // 读到 Nil → 守卫恒假、`when` 完全失效（fixture match_guard.mora
        // 只因取值恰好让首守卫为真而"通过"）。
        if let Some(guard) = guard_func {
            let g = run_mir(
                &std::sync::Arc::new((**guard).clone()),
                interp,
                env,
                effects,
            )?;
            if !is_truthy(&g) {
                continue;
            }
        }
        let result = run_mir(
            &std::sync::Arc::new((**arm_func).clone()),
            interp,
            env,
            effects,
        )?;
        regs[*output_reg] = result;
        matched = true;
        break;
    }
    if !matched && let Some((_pat, _guard, _func, output_reg)) = arms.first() {
        regs[*output_reg] = Value::Nil;
    }
    Ok(())
}

pub fn h_jump(target: usize) -> super::Flow {
    super::Flow::Jump(target)
}

pub fn h_jump_if(regs: &[Value], cond: Reg, target: usize) -> super::Flow {
    if is_truthy(&regs[cond]) {
        super::Flow::Jump(target)
    } else {
        super::Flow::Continue
    }
}

pub fn h_jump_if_not(regs: &[Value], cond: Reg, target: usize) -> super::Flow {
    if !is_truthy(&regs[cond]) {
        super::Flow::Jump(target)
    } else {
        super::Flow::Continue
    }
}

pub fn h_return(regs: &[Value], value: Option<Reg>) -> super::Flow {
    super::Flow::Return(value.map_or(Value::Nil, |r| regs[r].clone()))
}

/// v0.70: Vote to halt (Pregel semantics). In a BSP context the engine
/// marks the current vertex as Halted; it won't be rescheduled unless a
/// Send arrives. In a linear context, equivalent to return.
pub fn h_halt(regs: &[Value], value: Option<Reg>) -> super::Flow {
    super::Flow::Halt(value.map(|r| regs[r].clone()))
}

pub fn h_break(target: usize) -> super::Flow {
    super::Flow::Jump(target)
}

pub fn h_continue(target: usize) -> super::Flow {
    super::Flow::Jump(target)
}

/// v0.88: Quasiquote handler — 按 segments 重组为 Mora 源码字符串。
///
/// - Quote(s)     → 直接拼接源码文字 `s`
/// - Unquote(r)   → 读取 `regs[r]`，经 Mora Display 格式化为代码字符串
///   例：`x` 的值是 `Int(3)` → 拼接 `"3"`；值是 `String("hello")` → 拼接 `"\"hello\""`
/// - UnquoteSplice(r) → 读取 `regs[r]`（期望为 List），每个元素经 Display
///   格式化后用 `", "` 连接，展开为源码片段
///   例：`[1, 2, 3]` → 拼接 `"1, 2, 3"`
///
/// 最终 `dst` 寄存器写入 `Value::Code(重组源码字符串)`，与 `quote(expr)` 返回类型一致。
pub fn h_quasiquote(
    regs: &mut [Value],
    dst: Reg,
    segments: &[crate::mir::QuasiquoteSegment],
) -> Result<(), String> {
    use crate::mir::QuasiquoteSegment::{Quote, Unquote, UnquoteSplice};

    let mut buf = String::new();
    for seg in segments {
        match seg {
            Quote(src) => buf.push_str(src),
            Unquote(r) => {
                buf.push_str(&value_to_string(&regs[*r]));
            }
            UnquoteSplice(r) => {
                let val = &regs[*r];
                match val {
                    Value::List(items) => {
                        let parts: Vec<String> = items.iter().map(value_to_string).collect();
                        buf.push_str(&parts.join(", "));
                    }
                    _ => return Err(format!("unquote_splice: expected List, got {:?}", val)),
                }
            }
        }
    }
    regs[dst] = Value::Code(buf);
    Ok(())
}

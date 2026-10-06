//! v0.104.6 D42：程序顶层的 `return` **静默终止整个程序**，吞掉其后所有语句。
//!
//! ## 现象（修前，真实 CLI `mora run` 实测，**全部 exit 0、零提示**）
//!
//! ```mora
//! print(111)     → 111.0
//! return 1
//! print(999)     → 从不执行
//! ```
//!
//! 用户以为写了「提前返回」，实际看到的是「程序成功结束」——
//! 退出码 0、无错误、无警告，**999 与其后的一切凭空消失**。
//! 与 D35（`let r = handle …` 让其后所有语句静默消失）同族。
//!
//! `if` / `for` / `with` 的体**内联**在程序顶层，故同样中招：
//!
//! ```mora
//! if c == 1 then
//!   return 7
//! end
//! print(888)      → 从不执行（修前 exit 0）
//! ```
//!
//! ## 根因
//!
//! `return` 此前**没有任何作用域检查** —— 同一函数里
//! `break` / `continue` 早就有守卫（`EmitContext::loop_stack` +
//! `"Break outside loop"` / `"Continue outside loop"`，见
//! `parser_v3/emit_definitions.rs`），唯独 `return` 直接
//! `emit(MirInst::Return(..))` 到当前寄存器空间。落在程序顶层时，
//! 那条 `Return` 就是**顶层函数**的返回 → 整个程序结束。
//!
//! ## 为什么能只改一行就判对
//!
//! 判据用 `EmitContext::is_program_top`，**只有 `ParserV3::new` 会置
//! `true`**。程序顶层是**唯一**不属于任何函数体的寄存器空间：task /
//! closure / macro / worker / transaction / observe / update 等体都是
//! `EmitContext::new()`（即 `false`），而 `if` / `for` / `while` 的体
//! **不换上下文**，故自动继承外层的值 ——
//!
//! * task 里的 `for` 体 → 仍在函数体里 → 放行；
//! * 顶层的 `for` 体 → 仍在程序顶层 → 拒绝。
//!
//! 故无需逐个分类「哪些块体是函数体」（那要审 20 处 `EmitContext::new()`
//! 站点，判错一处就会误拒合法代码）。
//!
//! ## 为什么不保留「顶层 return = 结束程序」
//!
//! 那与「正常跑完」在**退出码和输出上完全不可区分**。宁可报错。
//!
//! ## 实测：合法 `return` 一处没被误伤（修后）
//!
//! | 作用域 | 结果 |
//! |---|---|
//! | `task` 体 | `7.0` ✓ |
//! | `fn` 闭包体 | `7.0` ✓ |
//! | `macro` 体 | ✓ |
//! | `worker` 体 | ✓ |
//! | `transaction` 体 / `compensation` 段 | ✓ |
//! | `observe` 体 | ✓ |
//! | `with` 体（实现里本就是独立函数体） | ✓ |
//! | `if` / `for` / `while` 体（**task 内**） | `7.0` ✓ |

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::value::Value;

fn compile(src: &str) -> Result<mora::mir::MirFunction, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    Ok(func)
}

fn run(src: &str) -> Result<Value, String> {
    let func = compile(src)?;
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

// ===================================================================
// 1) 程序顶层的 return 必须报错，不得静默吞掉后续语句
// ===================================================================

/// 这些源码修前全部 **exit 0 且把后续语句吞掉**。
///
/// 断言的是「编译期就拒绝」—— 这正是修前缺失的那道检查。断言编译错误
/// 而非运行结果，是因为吞语句这个现象在函数级 API 里只能通过
/// 「后续副作用有没有发生」来观察，而 `print` 捕获在此层拿不到。
///
/// 注：详细诊断走 stderr（`eprintln!`，与 parser 其余错误如
/// `Expected orchestrate kind` 同一形状），不进 `Err` 串，故此处只断言
/// 「被拒绝」，不断言文案。
#[test]
fn return_at_program_top_is_rejected() {
    let cases: &[(&str, &str)] = &[
        ("裸 return", "return 1\nprint(999)\n"),
        ("return 带值", "return 7\nprint(999)\n"),
        ("前面有语句", "print(111)\nreturn 1\nprint(999)\n"),
        // 内联块体同样中招：体不换 EmitContext，仍属程序顶层
        (
            "if 体内",
            "let c = 1\nif c == 1 then\n  return 7\nend\nprint(888)\n",
        ),
        ("for 体内", "for i in [0,1]\n  return 7\nend\nprint(888)\n"),
        ("裸 return 无值", "return\nprint(999)\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if compile(src).is_ok() {
            failures.push(format!("  [{name}] 编译通过（应拒绝）"));
        }
    }
    assert!(
        failures.is_empty(),
        "程序顶层的 `return` 必须编译期报错。修前它们全部静默终止程序、\
         吞掉后续语句且 exit 0：\n{}",
        failures.join("\n")
    );
}

// ===================================================================
// 2) 函数作用域内的 return 必须一处不误伤
// ===================================================================

/// 合法 `return` 的返回值经由**函数返回值**可观察 —— 这是「return 真的
/// 跑到了」的证据，而不是只看编译通过。
#[test]
fn return_inside_functions_still_works() {
    let cases: &[(&str, &str, f64)] = &[
        // (名称, 源码, 期望返回值)
        ("task 体", "task w()\n  return 7\nend\nw()\n", 7.0),
        (
            "task 内 if 体内",
            "task w()\n  if true then\n    return 7\n  end\n  9\nend\nw()\n",
            7.0,
        ),
        (
            "task 内 for 体内",
            "task w()\n  for i in [0]\n    return 7\n  end\n  9\nend\nw()\n",
            7.0,
        ),
        (
            "task 内 while 体内",
            "task w()\n  let i = 0\n  while i < 3\n    assign i = i + 1\n    return 7\n  end\n  9\nend\nw()\n",
            7.0,
        ),
        ("闭包体", "let f = fn(x) return 7 end\nf(0)\n", 7.0),
        (
            "task 内 if 体后续语句被跳过",
            "task w()\n  if true then\n    return 7\n  end\n  9\nend\nw()\n",
            7.0,
        ),
    ];
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        match run(src) {
            // 按数值比较 —— `Value::Float(7.0)` 的 Debug 是 `7`，不是 `7.0`
            Ok(Value::Float(got)) => {
                if got != *want {
                    failures.push(format!("  [{name}] 期望 {want}，实得 {got}"));
                }
            }
            Ok(other) => failures.push(format!("  [{name}] 期望 Float({want})，实得 {other:?}")),
            Err(e) => failures.push(format!("  [{name}] 运行失败: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "函数作用域内的 `return` 必须照常工作（D42 的守卫不得误伤）：\n{}",
        failures.join("\n")
    );
}

/// 其它函数体形态（macro / worker / transaction / observe）也必须不受影响。
///
/// 这些体的 `return` 修前就是正确的 —— 这里钉住的是「D42 的守卫没有把
/// 它们误判成程序顶层」。
#[test]
fn return_in_other_function_bodies_is_not_rejected() {
    let cases: &[(&str, &str)] = &[
        ("macro 体", "macro m(x)\n  return 7\nend\n1\n"),
        ("worker 体", "worker w\n  return 7\nend\n1\n"),
        ("transaction 体", "transaction\n  return 7\nend\n1\n"),
        (
            "transaction compensation 段",
            "transaction\n  1\n  compensation\n    return 7\nend\n1\n",
        ),
        ("observe 体", "observe tr do\n  return 7\nend\n1\n"),
        ("with 体", "with model = \"gpt-4o\"\n  return 7\nend\n1\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if let Err(e) = compile(src) {
            failures.push(format!("  [{name}] 编译失败: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "这些块体在实现里都是独立函数体（另开 EmitContext），\
         其 `return` 必须继续被接受：\n{}",
        failures.join("\n")
    );
}

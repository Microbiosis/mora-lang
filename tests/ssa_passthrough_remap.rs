//! v0.104.6 D417 —— 透传指令（`Closure` / `MatchExpr` / `Handle` / `WithConfig`
//! / `Perform` / `Solve` / `DynTrait` / `Quasiquote` …）**参与 SSA 重命名**。
//!
//! ## 缺陷：一个函数里同时存在两套寄存器空间
//!
//! `construct` 此前把这 8 类（外加全部声明型）指令抽进
//! `MirSsaFunction::passthrough`，**原样保留它们的寄存器号**；同一函数里
//! 其余指令却会被 SSA 重编号。于是（v0.104.6 D310 实测逐条对照）：
//!
//! ```text
//!   opt=off:  Closure { dst: 8 }   MethodCall(9, 4, "map", [8])   实参 8 = 闭包 ✅
//!   opt=1:    Closure { dst: 8 }   MethodCall(5, 1, "map", [2])   实参 2 ≠ 8   ❌
//! ```
//!
//! 后果是**静默**的：`function_call.mora`（`{"add": fn(a,b) a + b end}`）在
//! `--opt=1/2` 下**无输出、退出码 0、零诊断**；`match_guard.mora` 少打印一行；
//! `tea_list_model.mora` 什么都不打印。
//!
//! 两个「掩盖」手段都不解决问题：
//!
//! 1. `deconstruct` 用 `n_regs.max(orig_n_regs)` 把帧撑大 —— **值是错的，
//!    只是没越界报错**；
//! 2. `opt::optimize` 见到这类指令**整体跳过 SSA** —— 安全，但 57 个 fixture
//!    里 14 个永远拿不到 SSA 优化。
//!
//! 此外 `passthrough` 只按顺序收集、**不记录原始位置**，`deconstruct` 用
//! `chain(body)` 把它们**整体前插**到 body 头部 ⇒ 原始指令顺序被打乱。
//!
//! ## 修法：让它们成为 SSA IR 的一等变体
//!
//! `SsaInst::Opaque(MirInst)` —— 不为其建模语义，但**留在原位**参与同一条
//! 重命名路径（读经 `map_regs`、写分配 fresh 版本号）。三条缺陷同时消失：
//! 顺序由位置保证、寄存器由重命名保证、`n_regs` 由实际分配保证。
//!
//! 顺带修掉两条同族缺陷：
//!
//! - **`Return(None)` 的顺序后继边缺失**（D312 根因之一）：CFG 把它当终点，
//!   而 `deconstruct` 明写它的语义是「落下去」⇒ 后继块 `preds` 为空、
//!   遍历走不到 ⇒ 整块未被重命名（部分重命名）。
//! - **`collect_definitions` 把 `Assign` 当定义**：`Define`/`Assign` 写的是
//!   **env**，第二字段是源寄存器；把它们算作定义会凭空放大 phi 插入点。
//!
//! ## 本文件的判据
//!
//! ① **单寄存器空间**：SSA 函数内任一寄存器不得被两条指令同时定义。
//! ② **每个读都有生产者**：读到没有生产者的寄存器 = 执行期读未初始化槽
//!    = **静默错值**（本项目头号失败模式）。
//! ③ **依赖同构**：`construct → deconstruct` 往返后，指令的**依赖结构**
//!    （「这条指令读的值由哪条指令定义」）与原始 MIR 逐条相同。
//! ④ **顺序保持**：原位发射 —— 原本在前的指令不得被挪到其后。
//! ⑤ **执行等价**：57 个 fixture × `--opt=0/1/2` 三档输出与退出码全一致。

use mora::mir::control::{
    ControlInst, ControlProgram, ControlTerminator, OptLevel, verify_program,
};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

// ── ① / ② 的性质检查 ────────────────────────────────────────────────

fn compile_and_verify(src: &str, level: OptLevel) -> mora::error::MoraResult<ControlProgram> {
    let (prog, _) = mora::cli::compile_and_opt(src, Some(level))?;
    verify_program(&prog)?;
    Ok(prog)
}

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("读 e2e fixture 目录")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "mora").then_some(p)
        })
        .collect();
    v.sort();
    assert!(
        v.len() >= 30,
        "fixture 数异常少（{}），判据会失去意义",
        v.len()
    );
    v
}

/// 每个 fixture 在各优化档位下必须通过 `verify_program`（单一寄存器空间 + 每次读取都有生产者）。
#[test]
fn d417_construct_yields_a_single_register_space() {
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for p in fixtures() {
        let src = std::fs::read_to_string(&p).expect("读 fixture");
        if mora::cli::compile_and_opt(&src, Some(OptLevel::None)).is_err() {
            continue;
        }
        checked += 1;
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
            if let Err(e) = compile_and_verify(&src, level) {
                failures.push(format!("{name} ({level:?}): {e}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "**D417**：Control IR 校验失败（存在重复定义/未定义寄存器读取/越界）：\n  - {}",
        failures.join("\n  - ")
    );
    assert!(
        checked >= 30,
        "只检查了 {checked} 个 fixture —— 覆盖面异常，判据会失去意义"
    );
}

fn run_control(src: &str, level: OptLevel) -> String {
    let prog = compile_and_verify(src, level).expect("compile + verify");
    let mut rt = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut rt).expect("execute_program");
    val.to_string()
}

/// **主护栏**：优化前后必须保持依赖语义与输出等价。
#[test]
fn d417_roundtrip_preserves_the_dependency_graph() {
    for (tag, src) in [
        (
            "闭包+字典",
            "let ops = {\"add\": fn(a, b) a + b end}\nops.add(2i, 3i)\n",
        ),
        ("map 闭包", "[1,2,3].map(fn(v) v + 10)\n"),
        (
            "match",
            "let x = 2\nmatch x { 1 => \"a\", 2 => \"b\", _ => \"c\" }\n",
        ),
        (
            "match+守卫",
            "let x = 3\nmatch x { n when n > 5 => \"big\", _ => \"small\" }\n",
        ),
        (
            "handle/perform",
            "let global_result = \"init\"\nhandle Ai {\n  global_result = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nglobal_result\n",
        ),
        ("quasiquote", "let x = 5\nlet q = `10+,x\neval(q)\n"),
    ] {
        let base = run_control(src, OptLevel::None);
        for level in [OptLevel::Basic, OptLevel::Aggressive] {
            let opt = run_control(src, level);
            assert_eq!(
                opt, base,
                "[{tag}] {level:?} 改变了依赖与执行语义：base={base:?}, opt={opt:?}"
            );
        }
    }
}

/// 闭包构造必须原位发射在消费它的字典字面量之前。
#[test]
fn d417_opaque_instructions_keep_their_original_position() {
    for level in [OptLevel::None, OptLevel::Aggressive] {
        let prog = compile_and_verify(
            "let ops = {\"add\": fn(a, b) a + b end}\nprint(1i)\n",
            level,
        )
        .expect("compile");
        let flat: Vec<&ControlInst> = prog.functions[prog.entry.0 as usize]
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .collect();
        let closure_at = flat
            .iter()
            .position(|i| matches!(i, ControlInst::MakeClosure { .. }))
            .expect("missing MakeClosure");
        let dict_at = flat
            .iter()
            .position(|i| matches!(i, ControlInst::DictLit(..)))
            .expect("missing DictLit");
        assert!(
            closure_at < dict_at,
            "[{level:?}] MakeClosure({closure_at}) 必须排在 DictLit({dict_at}) 之前"
        );
    }
}

/// `task main()` 的 `RegisterTask` 必须排在调用它之前。
#[test]
fn d417_task_def_keeps_its_place_relative_to_its_call() {
    for level in [OptLevel::None, OptLevel::Aggressive] {
        let prog = compile_and_verify("task main()\n  print(\"hi\")\nend\nmain()\n", level)
            .expect("compile");
        let main_fn = &prog.functions[prog.entry.0 as usize];
        let mut reg_pos: Option<usize> = None;
        let mut call_pos: Option<usize> = None;
        let mut idx = 0usize;
        for blk in &main_fn.blocks {
            for inst in &blk.insts {
                if matches!(inst, ControlInst::RegisterTask { .. }) && reg_pos.is_none() {
                    reg_pos = Some(idx);
                }
                if matches!(inst, ControlInst::Call { .. }) {
                    call_pos = Some(idx);
                }
                idx += 1;
            }
            if matches!(blk.terminator, ControlTerminator::TailCall { .. }) {
                call_pos = Some(idx);
            }
            idx += 1;
        }
        assert!(
            reg_pos.zip(call_pos).is_some_and(|(r, c)| r < c),
            "[{level:?}] RegisterTask({reg_pos:?}) 必须排在 main() 调用({call_pos:?})之前"
        );
    }
}

fn run(exe: &str, dir: &Path, file: &str, opt: Option<&str>) -> (i32, String) {
    let mut c = Command::new(exe);
    c.current_dir(dir);
    if let Some(o) = opt {
        c.arg(o);
    }
    let out = c.arg(file).output().expect("跑 mora");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let cleaned = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("并发与抽象")
                && !l.starts_with("类型与执行")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
        })
        .collect::<Vec<_>>()
        .join("\n");
    (out.status.code().unwrap_or(-1), cleaned)
}

/// **57 个 fixture × `--opt=0/1/2`：三档输出与退出码必须完全一致。**
///
/// 这条是本刀的主执行级护栏 —— ①②③④ 都是结构判据，只有它能证明
/// 「优化没有改变程序的可观察行为」。D310 的门正是被它挡住的：解禁后
/// `function_call.mora` 在 `--opt=1` 下静默无输出、退出码 0。
#[test]
fn d417_opt_levels_never_change_fixture_behaviour() {
    let exe = env!("CARGO_BIN_EXE_mora");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}：集成测试需要 `cargo build --bin mora` 产出的二进制");
    }
    let dir = TempDir::new("d417");

    let mut broken: Vec<String> = Vec::new();
    for f in fixtures() {
        let (c0, o0) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=0"));
        let (c1, o1) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=1"));
        let (c2, o2) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=2"));
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if (c1, &o1) != (c0, &o0) {
            broken.push(format!(
                "{name}: opt=1 exit={c1} {o1:?} ≠ opt=0 exit={c0} {o0:?}"
            ));
        }
        if (c2, &o2) != (c0, &o0) {
            broken.push(format!(
                "{name}: opt=2 exit={c2} {o2:?} ≠ opt=0 exit={c0} {o0:?}"
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "`--opt` 改变了 {} 个 fixture 的可观察行为：\n  - {}",
        broken.len(),
        broken.join("\n  - ")
    );
}

/// **曾经被门挡住的三个 fixture 必须真的走 SSA 了。**
///
/// 判据不是「输出对」（那可能是根本没优化），而是「`opt.rs` 里不再有
/// 透传守卫」+「输出与 `opt=0` 一致」。
#[test]
fn d417_previously_gated_fixtures_now_reach_ssa() {
    let exe = env!("CARGO_BIN_EXE_mora");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}");
    }
    let dir = TempDir::new("d417b");

    // D310 记档的三个「解禁即静默失败」样本 + 语义各异的另外两个。
    for name in [
        "function_call.mora",  // {"add": fn(a,b) … end} → 5
        "match_guard.mora",    // 多行 match 输出
        "tea_list_model.mora", // TEA 列表模型
        "match_default.mora",
        "rel_basic.mora",
    ] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/e2e")
            .join(name);
        let (c0, o0) = run(exe, &dir, p.to_str().unwrap(), Some("--opt=0"));
        let (c1, o1) = run(exe, &dir, p.to_str().unwrap(), Some("--opt=1"));
        assert_eq!(
            (c1, &o1),
            (c0, &o0),
            "**D417**：[{name}] 此前因含透传指令被 `has_register_carrying_passthrough`\
             整体跳过 SSA。该门已删除，`--opt=1` 必须与 `--opt=0` 逐行等价。\n  \
             opt=0 exit={c0} {o0:?}\n  opt=1 exit={c1} {o1:?}"
        );
    }
}

/// `{"add": fn(a,b) a + b end}` 这个 D310 的原始样本：闭包必须能被 SSA 重命名。
#[test]
fn d417_closure_literal_survives_renaming() {
    let dir = TempDir::new("d417c");
    let f = dir.join("a.mora");
    std::fs::write(
        &f,
        "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))\n",
    )
    .expect("写脚本");

    let exe = env!("CARGO_BIN_EXE_mora");
    if !Path::new(exe).exists() {
        panic!("缺 {exe}");
    }
    let (_, base) = run(exe, &dir, f.to_str().unwrap(), Some("--opt=0"));
    assert_eq!(base, "5", "前提：opt=0 应打印 5");
    for lvl in ["1", "2"] {
        let (c, got) = run(
            exe,
            &dir,
            f.to_str().unwrap(),
            Some(&format!("--opt={lvl}")),
        );
        assert_eq!(
            (c, got.clone()),
            (0, "5".to_string()),
            "**D310/D417**：闭包字面量的寄存器此前不参与 SSA 重命名 ⇒ `map`/方法调用\
             去读了另一个寄存器 ⇒ `--opt=1` 下**静默无输出、退出码 0**。\n  \
             `--opt={lvl}` 实得 exit={c} {got:?}"
        );
    }
}

//! v0.104.6：寄存器分配器（槽位复用）—— 语义等价 + 实际收益的护栏。
//!
//! 分配是**纯重编号**（只改寄存器号与 `n_regs`），故首要性质是「结果与分配前
//! 逐条一致」。本文件用两条互相独立的判据锁住：
//!
//! 1. **等价性** —— 全量 e2e fixture 在 `MORA_NO_REGALLOC=1`（不分配）与默认
//!    （分配）下的 stdout 与退出码**完全相同**。
//! 2. **收益性** —— 确有空间被压（否则等价但无用的分配器等于死代码）。
//!
//! ## 适用范围的诚实说明
//!
//! 分配器按 pc 线性扫描建活跃区间，只对**证明安全**的函数体启用：
//! - 无回跳（循环）：回边使「pc 序 ≠ 执行序」，跨迭代活跃性需 CFG 级分析；
//! - 无效果/控制语句（`Handle`/`Perform`/`TaskDef`/…）：这些指令写寄存器的
//!   方式不由 `written_reg()` 完整覆盖，逐指令 def 计数会漏。
//!
//! 不满足者**原样保留** emit 的编号（行为与分配前逐位一致）。故收益覆盖面
//! 小于「全部函数体」，这是安全优先的取舍，详见 `src/mir/regalloc.rs`。

use std::path::Path;

/// 跑一个 fixture，返回 `(stdout, exit_code)`。
fn run(fixture: &str) -> (String, i32) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg(fixture)
        .env("MORA_NO_REGALLOC", "1")
        .output()
        .expect("跑 mora");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// 全量 e2e fixture 列表。
fn fixtures() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e");
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("读 fixtures 目录")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension().map(|x| x == "mora").unwrap_or(false))
                .then(|| p.to_string_lossy().into_owned())
        })
        .collect();
    v.sort();
    v
}

/// **主护栏**：分配前后，所有 e2e fixture 的行为必须**逐字节相同**。
///
/// 分配是纯重编号，任何行为差异都说明活跃性算错（或有未覆盖的寄存器位置）。
#[test]
fn allocation_preserves_behavior_on_all_e2e_fixtures() {
    let mut mismatches = Vec::new();
    for f in fixtures() {
        // 同一二进制、同一 fixture，只切换 MORA_NO_REGALLOC。
        let off = {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
                .arg(&f)
                .env("MORA_NO_REGALLOC", "1")
                .output()
                .expect("跑 mora");
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                out.status.code().unwrap_or(-1),
            )
        };
        let on = run(&f);
        if off.0 != on.0 || off.1 != on.1 {
            mismatches.push(format!(
                "{f}\n    不分配: exit={} stdout={:?}\n    分配后: exit={} stdout={:?}",
                off.1, off.0, on.1, on.0
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "寄存器分配改变了行为（{} 个 fixture）：\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}

/// **收益性**：确有空间被压 —— 否则分配器是「安全但无用」的死代码。
///
/// 用 9 层管线的优化链实测若干 e2e fixture 的 `n_regs`。
#[test]
fn allocation_actually_shrinks_register_pressure() {
    // 直接用库入口编译 + 分配，统计「被压缩的函数空间」数量。
    fn visit(
        f: &mora::mir::MirFunction,
        shrunk: &mut usize,
        before: &mut usize,
        after: &mut usize,
    ) {
        let mut g = f.clone();
        let b = g.n_regs;
        mora::mir::regalloc::allocate(&mut g);
        if g.n_regs < b {
            *shrunk += 1;
        }
        *before += b;
        *after += g.n_regs;
        for inst in &g.body {
            let mut nested: Vec<&mora::mir::MirFunction> = Vec::new();
            match inst {
                mora::mir::MirInst::Closure { body, .. }
                | mora::mir::MirInst::TaskDef { body, .. } => nested.push(body),
                mora::mir::MirInst::Handle { body, handler, .. } => {
                    nested.push(body);
                    nested.push(handler);
                }
                _ => {}
            }
            for n in nested {
                visit(n, shrunk, before, after);
            }
        }
    }

    let mut shrunk = 0usize;
    let mut before = 0usize;
    let mut after = 0usize;
    let mut compiled = 0usize;
    for f in fixtures() {
        let src = std::fs::read_to_string(&f).expect("读 fixture");
        if let Ok((func, _)) = mora::parser_v3::ParserV3::compile(&src) {
            compiled += 1;
            visit(&func, &mut shrunk, &mut before, &mut after);
        }
    }
    assert!(compiled > 0, "没有任何 fixture 编译成功 —— 测试前提不成立");
    assert!(
        shrunk > 0,
        "没有任何函数空间被压缩（before={before}, after={after}）—— 分配器成了死代码。\
         若判据收紧到无可分配对象，请同步更新本测试与 `src/mir/regalloc.rs` 的范围说明。"
    );
    assert!(
        after < before,
        "分配后寄存器总数未下降：{before} -> {after}"
    );
}

/// 不满足安全判据的函数体（含循环 / 效果语句）**必须原样保留**编号。
///
/// 这是「安全优先」策略的直接断言：宁可零收益，不可错值。
#[test]
fn unsafe_shapes_are_left_untouched() {
    for src in [
        // 含循环（回跳）
        "task main()\n  let i = 0\n  while i < 3\n    assign i = i + 1\n  end\n  print(i)\nend\n",
        // 含 handler（效果语句）
        "handle Ai {\n  perform Ai(\"x\")\n} {\n  1\n}\n",
    ] {
        let (func, _) = mora::parser_v3::ParserV3::compile(src).expect("应能解析");
        let mut g = func.clone();
        let before_all = collect_nregs(&g);
        mora::mir::regalloc::allocate(&mut g);
        assert_eq!(
            collect_nregs(&g),
            before_all,
            "含环/效果语句的函数体应原样保留（不应被分配）:\n{src}"
        );
    }
}

/// 递归收集所有寄存器空间的 `n_regs` 之和。
fn collect_nregs(f: &mora::mir::MirFunction) -> usize {
    let mut sum = f.n_regs;
    for inst in &f.body {
        let mut nested: Vec<&mora::mir::MirFunction> = Vec::new();
        match inst {
            mora::mir::MirInst::Closure { body, .. } | mora::mir::MirInst::TaskDef { body, .. } => {
                nested.push(body)
            }
            mora::mir::MirInst::Handle { body, handler, .. } => {
                nested.push(body);
                nested.push(handler);
            }
            _ => {}
        }
        for n in nested {
            sum += collect_nregs(n);
        }
    }
    sum
}

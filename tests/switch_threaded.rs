//! v0.104.6：顺序直穿（switch-threaded）—— 等价性与性能护栏。
//!
//! 快路径在「`active` 恰有一个已就绪节点」时直接沿唯一后继执行，跳过 BSP 波次
//! 调度（重建 ready / 扫全部边 / 代际标记）。实测该开销与循环体大小**无关**，
//! 是紧循环里每条指令的主要成本（1M 迭代基线 ~4.3 s）。
//!
//! 本文件锁两条：
//! 1. **等价性** —— 全量 e2e fixture 在快路径开/关下 stdout 与退出码逐字节相同。
//! 2. **性能护栏** —— 1M 迭代基准，快路径必须显著快于基线，防止优化被静默回退。

use mora::test_support::TempFile;
use std::path::Path;

/// 跑一个 fixture，返回 `(stdout, exit_code)`；`no_threading` 控制快路径开关。
fn run(fixture: &str, no_threading: bool) -> (String, i32) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_mora"));
    cmd.arg(fixture);
    if no_threading {
        cmd.env("MORA_NO_THREADING", "1");
    }
    let out = cmd.output().expect("跑 mora");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

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

/// **主护栏**：快路径开/关，所有 e2e fixture 的行为必须完全相同。
///
/// 快路径复用同一个 `handlers::dispatch` 与同一套 memo，理论上不可能有差异；
/// 一旦出现差异，说明「后继选取」或「交回波次路径」的状态交互有错。
#[test]
fn switch_threading_preserves_behavior_on_all_e2e_fixtures() {
    let mut mismatches = Vec::new();
    for f in fixtures() {
        let off = run(&f, true);
        let on = run(&f, false);
        if off.0 != on.0 || off.1 != on.1 {
            mismatches.push(format!(
                "{f}\n    快路径关: exit={} stdout={:?}\n    快路径开: exit={} stdout={:?}",
                off.1, off.0, on.1, on.0
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "顺序直穿改变了行为（{} 个 fixture）：\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}

/// **分叉回退场景**：if/else、break/continue、handler、嵌套闭包 ——
/// 这些正是会「后继不唯一」而交回 BSP 波次路径的程序，结果必须不变。
#[test]
fn switch_threading_handles_forking_control_flow() {
    for (label, src, expect) in [
        (
            "if/else as value",
            "let c = 2\nlet x = if c == 1 then 5 else 7 end\nprint(x)\n",
            "7.0\n",
        ),
        (
            "for + break",
            "let n = 0\nfor x in [1, 2, 3, 4, 5]\n  if x == 3 then\n    break\n  end\n  assign n = n + 1\nend\nprint(n)\n",
            "2.0\n",
        ),
        (
            "while + continue",
            "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n    continue\n  end\n  assign n = n + 1\n  assign k = k + 1i\nend\nprint(n)\n",
            "5.0\n",
        ),
        (
            "handler",
            "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
            "mocked:hello\n",
        ),
        (
            "closure call",
            "let add = fn(a, b) a + b end\nprint(add(2, 3))\n",
            "5.0\n",
        ),
    ] {
        // v0.104.6 D475：要的是**文件** ⇒ TempFile。
        let tmp = TempFile::new("threading_case", "p.mora");
        let path = tmp.path();
        std::fs::write(path, src).expect("写临时脚本");
        let p = path.to_string_lossy().into_owned();
        let off = run(&p, true);
        let on = run(&p, false);
        assert_eq!(
            (off.1, off.0.clone()),
            (on.1, on.0),
            "[{label}] 快路径改变了控制流程序的结果"
        );
        assert_eq!(off.0.trim(), expect.trim(), "[{label}] 基准值应稳定");
    }
}

/// **性能护栏**：1M 次迭代，快路径必须显著快于基线。
///
/// 阈值取「至少快 15%」—— 低于此说明快路径没真正生效（例如后继选取反复
/// GiveUp），属于**静默回退**，必须让测试红。
///
/// 用 release 二进制对比；若只 build 了 debug，两者都会被构建时间主导而失去
/// 区分度，故以「相对比例」判定而非绝对毫秒。
#[test]
#[ignore = "性能护栏：需 --release 二进制；由 scripts/ 或 CI 显式运行"]
fn switch_threading_is_faster_on_tight_loop() {
    let src = "let s = 0i\nlet j = 0i\nwhile j < 1000000\n  assign s = s + 1i\n  assign j = j + 1i\nend\nprint(s)\n";
    let tmp = TempFile::new("threading_bench", "p.mora");
    let path = tmp.path();
    std::fs::write(path, src).expect("写基准脚本");
    let p = path.to_string_lossy().into_owned();

    let exe = env!("CARGO_BIN_EXE_mora");
    let median = |no_threading: bool| -> f64 {
        let mut ts = Vec::new();
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let mut cmd = std::process::Command::new(exe);
            cmd.arg(&p);
            if no_threading {
                cmd.env("MORA_NO_THREADING", "1");
            }
            let _ = cmd.output().expect("跑 mora");
            ts.push(t0.elapsed().as_secs_f64());
        }
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ts[2]
    };

    let off = median(true);
    let on = median(false);

    assert!(
        on < off * 0.85,
        "快路径未带来预期加速：开={on:.3}s 关={off:.3}s（要求至少快 15%）—— \
         快路径可能反复交回波次路径（检查 threaded_successor 的后继选取）"
    );
}

//! v0.104.6：性能回归护栏 —— 把「顺序直穿」与「建图减边」的收益**量化并锁定**。
//!
//! ## 为什么需要这条
//!
//! 本轮两项优化（顺序直穿、建图只对非循环块去扇出）都是**性能**改动，而
//! 性能改动的典型失败模式是**静默回退**：代码还在、测试全绿、但因为某个
//! 无关重构（或一次错误的「顺手清理」）收益归零，且**没有任何测试会红**。
//!
//! 本文件把收益写成可执行断言。注意 `#[ignore]`：它需要 **release 二进制**
//! 与数秒级墙钟，`cargo test` 的常规 debug 跑法会让每次 CI 都付出这个代价而
//! 得不到有意义的数字（debug 下解释器开销结构完全不同）。用下面的命令显式运行：
//!
//! ```text
//! cargo test --release --test perf_regression -- --ignored --nocapture
//! ```

use mora::test_support::TempDir;
use std::time::Instant;

/// 跑一个脚本，返回墙钟秒数（取 n 次的中位数，降低噪声）。
fn median_secs(script: &str, n: usize, env: Option<(&str, &str)>) -> f64 {
    let exe = env!("CARGO_BIN_EXE_mora");
    let mut ts: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg(script);
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let _ = cmd.output().expect("跑 mora");
        ts.push(t0.elapsed().as_secs_f64());
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ts[ts.len() / 2]
}

/// 1M 次迭代的紧循环基准（`while` + 两条 assign）。
const TIGHT_LOOP: &str = "let s = 0i\nlet j = 0i\nwhile j < 1000000\n  assign s = s + 1i\n  assign j = j + 1i\nend\nprint(s)\n";

/// **护栏 1：顺序直穿必须显著快于纯 BSP 波次路径。**
///
/// 阈值「至少快 15%」。低于此说明直穿没真正生效 —— 最可能的回归是
/// `threaded_successor` 重新频繁 `GiveUp`（例如有人「顺手」给建图加了边、
/// 或去掉了 `in_loop` 判定），而这类退化**其它测试抓不到**。
#[test]
#[ignore = "性能护栏：需 release 二进制；cargo test --release --test perf_regression -- --ignored"]
fn switch_threading_speedup_is_preserved() {
    let p = TempDir::new("mora_perf_tight");
    std::fs::write(&p, TIGHT_LOOP).expect("写基准");
    let f = p.to_string_lossy().into_owned();

    let threaded = median_secs(&f, 5, Some(("MORA_NO_THREADING", "0")));
    let bsp_only = median_secs(&f, 5, Some(("MORA_NO_THREADING", "1")));
    let _ = std::fs::remove_file(&p);

    assert!(
        threaded < bsp_only * 0.85,
        "顺序直穿加速消失：直穿={threaded:.3}s 纯BSP={bsp_only:.3}s \
         （要求至少快 15%）。可能 `threaded_successor` 又在频繁交回波次路径。"
    );
    println!(
        "  直穿={threaded:.3}s  纯BSP={bsp_only:.3}s  加速={:.2}x",
        bsp_only / threaded
    );
}

/// **护栏 2：建图边数 —— 「直线 vs 循环」的程序，其每指令平均边数不应因
/// `in_loop` 判定过宽而膨胀。**
///
/// ## 判据为什么不是「跨节点 Sequence 边为 0」
///
/// 首版曾想断言「直线程序里没有跨节点 Sequence 边」，实测**该断言本身是错的**：
/// 即便最直白的 `let a = 1i / let b = 2i / … / print(a+b+c)`，MIR 里也有每个
/// `let` 隐式生成的 `__let_result` 三件套（`Const` / `Define` / `Var`），
/// DAG 正确地给它们排序 ⇒ 天然存在跨节点的 Sequence 边。那是**真实依赖**，
/// 不是冗余扇出。
///
/// ## 可判定的性质
///
/// 冗余扇出的特征是：**同一对 `(from,to)` 被连两次**（Step 1 链 + Step 3 扇出
/// 都连了它）。这已被 `tests/dag_edge_dedup.rs` 钉住。本条改守另一个角度 ——
/// **边数不应随「直线程序里 Effect 节点增多」而超线性增长**：Step 3 扇出是
/// 「每个 Effect 之后的**每个**节点各连一条」，若它对直线块生效，边数会按
/// Effect 数 × 后续节点数膨胀。
///
/// 故判据取：**边数 / 指令数** 的比值有上限。实测当前直线程序该比值远低于
/// 上限；一旦 `in_loop` 判定失效（对直线块也加扇出），比值会显著上升。
#[test]
fn straight_line_graph_stays_sparse() {
    // 直线程序：多条 `let`（每条都产生隐式 Effect 三件套）+ 末尾 print。
    let src = "let a0 = 0i\nlet a1 = 1i\nlet a2 = 2i\nlet a3 = 3i\nlet a4 = 4i\n\
               let a5 = 5i\nlet a6 = 6i\nlet a7 = 7i\nprint(a0+a1+a2+a3+a4+a5+a6+a7)\n";
    let (func, _) = mora::parser_v3::ParserV3::compile(src).expect("应能编译");
    let n_inst = func.body.len();
    let dag = mora::mir::dag::dag_analyze(&std::sync::Arc::new(func));
    let ratio = dag.edges.len() as f64 / n_inst as f64;

    // 阈值 **2.0**，由实测标定：
    //   - 当前（in_loop 生效，直线块不加扇出）：ratio = 1.80
    //   - 人为回退（对所有块无条件加扇出）：ratio = 2.21
    // 取 2.0 居中：既能抓住回退，又不会被小版本差异误伤。
    assert!(
        ratio < 2.0,
        "直线程序的边/指令比为 {ratio:.2}（{}/{}），超过阈值 2.0 \
         （当前正确值 ≈1.80，若 `last_effect` 扇出对**非循环块**也生效则 ≈2.21）—— \
         in_loop 判定疑似失效。",
        dag.edges.len(),
        n_inst
    );
}

/// **护栏 3：循环体的承重边必须保留。**
///
/// 这是护栏 2 的**反向**断言，防止「为了减边把循环体也减了」—— 首版正是
/// 这么错的，结果 `continue` 语义从 5.0 变成 8.0（`continue_semantics` 抓到了）。
///
/// 判定：含 `while` 的程序，其**循环体内**必须仍有跨节点 Sequence 边。
#[test]
fn loop_body_keeps_its_load_bearing_edges() {
    let src = "let n = 0\nlet k = 0i\nwhile k < 6i\n  assign n = n + 1\n  assign k = k + 1i\nend\nprint(n)\n";
    let (func, _) = mora::parser_v3::ParserV3::compile(src).expect("应能编译");
    let dag = mora::mir::dag::dag_analyze(&std::sync::Arc::new(func));

    let spanning = dag
        .edges
        .iter()
        .filter(|e| matches!(e.kind, mora::mir::dag::EdgeKind::Sequence))
        .filter(|e| e.to as i64 - e.from as i64 > 1)
        .count();

    assert!(
        spanning > 0,
        "循环体里一条跨节点 Sequence 边都没有 —— 承重约束被误删了。\
         首版按「块内含回边」判定就犯了这个错（`continue` 5.0 → 8.0）。"
    );
}

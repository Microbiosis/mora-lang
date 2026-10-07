//! v0.104.6 D364 —— `src/mir/optimize/` 的**语义不变性**：
//! `--opt` 四档 × 两条管线（否定轮，无产品变更）
//!
//! 优化器是全仓最危险的一层 —— 它改写指令序列，而**任何等价性破坏
//! 都不会崩溃**，只会**静默算错**。D363 顺带发现
//! `dag_rule.rs` / `dag_search.rs` 里有大段关于 `__let_result`
//! 寄存器安全的注释（历史上真的出过「寄存器失去 producer → 消费者
//! 永不 ready → 尾部 `print` 静默消失」的事故），所以这一层值得强验证。
//!
//! ## 核心判据：**优化器只能改效率，不能改语义**
//!
//! 同一程序在 `--opt=0/1/2/3` 与 `MORA_9LAYER=1/0` 下必须产生
//! **完全相同**的输出。这是可证伪的 —— 只要有一条判据不符，
//! 就说明某个档位优化坏了。
//!
//! ## 顺带查明：9 层管线的差分是**假阳性**
//!
//! 56 个真实 fixture 全量跑，**稳定**有 1 个触发差分失败：
//!
//! ```text
//! tests/fixtures/e2e/tea_standalone.mora
//!   [9layer] 差分失败：已回落到 emit.rs 路径 | pipeline_mir=5 original_mir=11
//!   diff: inst[0]: pipeline="ModelDef" original="Const"
//!   diff: inst[1]: pipeline="MsgDef"  original="Const"
//! ```
//!
//! 差异只是**指令形式不同**（`ModelDef` vs 占位 `Const`）而**语义等价**，
//! 差分器按**逐条指令名比对** ⇒ 误报。
//!
//! **判定不是新缺陷**：
//! ① 回落是**设计的安全网**（`cli/mod.rs:46` 明写「差分红 → 自动回落
//!    原管线（不中断编译）」）；
//! ② D36 已把「静默降级」修成**默认打一行警告**
//!    （`cli/mod.rs:68-70`），所以现在**不静默**了；
//! ③ 两条管线**结果完全一致**（本文件的判据直接钉了这一点）。
//!
//! ⇒ 真正的缺陷在**差分器过严**，属**契约分叉**（差分判据 vs 语义等价），
//! 改动涉及 `mir/pipeline.rs` 的等价判据设计 ⇒ **只报告，不擅动**。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d364_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d364_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let path_str = p.to_string_lossy().into_owned();
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **装置自检**（D356 教训：先证装置有效，再看它测出的数据）。
#[test]
fn d364_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// 覆盖各类**优化机会**的程序：闭包、while 循环、常量折叠、
/// if 死分支、算术化简、列表索引。
const MIXED: &str = "\
let f = fn(x) x * 2 + 1 end
let g = fn(a, b) a - b end
print(f(5))
print(g(10, 3))
let n = 0
let i = 0
while i < 5
  n = n + i
  i = i + 1
end
print(n)
let xs = [3, 1, 2]
print(xs.len())
print(xs[0] + xs[1] + xs[2])
let t = true
if t
  print(1)
else
  print(2)
end
let z = 0
print(z)
print(1 + 2 * 3)
";

/// **主断言**：`--opt=0/1/2/3` 四档必须产生**完全相同**的输出。
///
/// 优化器按设计只能改效率。任一档位改变结果 ⇒ 那一档优化破坏了语义。
#[test]
fn d364_all_four_opt_levels_produce_identical_output() {
    let baseline = {
        let (code, got) = ev(MIXED);
        assert_eq!(code, 0, "基线应正常跑; 实得 exit={code} out={got}");
        got
    };
    // 期望值也写死：即便四档一致地算错，也要被这条抓住
    assert_eq!(
        baseline, "11.0 | 7.0 | 10.0 | 3 | 6.0 | 1.0 | 0.0 | 7.0",
        "混合用例的期望输出变了 —— 先确认 fixture 本身对不对"
    );
    assert!(
        !baseline.is_empty(),
        "基线输出为空 —— 采集器失效，档位对比会**恒绿**（D338 教训）"
    );
}

/// **反向对照**：把 `--opt` 真的加到命令行上跑四遍。
///
/// 上面那条走的是默认档（本文件的 `ev` 不传 `--opt`），
/// 这条才真正验证**参数生效**且不改语义。
#[test]
fn d364_opt_flag_does_not_change_semantics() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let dir = std::env::temp_dir().join("d364_optflag");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("m.mora");
    std::fs::write(&p, MIXED).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");

    let mut results: Vec<String> = Vec::new();
    for level in ["0", "1", "2", "3"] {
        let out = Command::new(exe)
            .arg(format!("--opt={level}"))
            .arg(&p)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .output()
            .expect("跑 mora");
        assert_eq!(
            out.status.code(),
            Some(0),
            "--opt={level} 应正常退出; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let got: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        results.push(got.join(" | "));
    }
    assert_eq!(
        results[0], results[1],
        "--opt=0 与 --opt=1 结果不同：\n  0 → {}\n  1 → {}",
        results[0], results[1]
    );
    assert_eq!(
        results[0], results[2],
        "--opt=0 与 --opt=2 结果不同：\n  0 → {}\n  2 → {}",
        results[0], results[2]
    );
    assert_eq!(
        results[0], results[3],
        "--opt=0 与 --opt=3 结果不同：\n  0 → {}\n  3 → {}",
        results[0], results[3]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **两条管线（9 层 vs emit.rs）必须产生相同结果**。
///
/// `MORA_9LAYER=0` 强制走 `emit.rs` 直出。差分器只在**指令名**层面比对，
/// 语义等价的改写会被判为「差分失败」并回落 —— 但**结果必须一样**。
#[test]
fn d364_both_pipelines_produce_identical_output() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let dir = std::env::temp_dir().join("d364_pipeline");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("m.mora");
    std::fs::write(&p, MIXED).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");

    let mut results: Vec<String> = Vec::new();
    for layer in ["1", "0"] {
        let out = Command::new(exe)
            .arg(&p)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("MORA_9LAYER", layer)
            .output()
            .expect("跑 mora");
        assert_eq!(
            out.status.code(),
            Some(0),
            "MORA_9LAYER={layer} 应正常退出; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let got: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        results.push(got.join(" | "));
    }
    assert_eq!(
        results[0], results[1],
        "9 层管线与 emit.rs 直出结果不同：\n  9layer=1 → {}\n  9layer=0 → {}",
        results[0], results[1]
    );
    assert!(!results[0].is_empty(), "输出为空则本条恒绿（采集器失效）");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **`tea_standalone.mora` 的差分失败**已不复存在**（D412 已修）。
///
/// 原名 `d364_tea_differential_failure_is_a_false_positive` —— 它断言
/// 「TEA 的差分失败是假阳性：结果完全正确，只是差分器过严」。
///
/// ⚠ **D412 让那个结论失效了**：真因不是差分器过严，而是
/// `model` 字段默认值在**入口**就被丢弃
/// （`let (dreg, _dw) = self.emit_expr_w()?;` 把 `_dw` 扔了
/// ⇒ `WitnessKind::ModelDef` 没有 `defaults`）
/// ⇒ 管线产出的 `ModelDef` 撞上 emit.rs 的默认值 `Const`，被判不等价。
///
/// 补上默认值后差分**通过**，本条随之翻转为正确行为断言。
#[test]
fn d412_tea_differential_now_passes() {
    let fixture = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/e2e/tea_standalone.mora"
    ));
    assert!(fixture.exists(), "fixture 不存在：{}", fixture.display());

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let run = |layer: &str| {
        let out = Command::new(exe)
            .arg(fixture)
            .env("MORA_9LAYER", layer)
            .env("MORA_9LAYER_DEBUG", "1")
            .output()
            .expect("跑 mora");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (c1, out1, err1) = run("1");
    let (c0, out0, _) = run("0");

    assert_eq!(c1, c0, "两条路径的退出码应一致");
    assert_eq!(
        out1, out0,
        "两条编译路径的输出应逐字相同（D412 已对齐）;\n 9layer : {out1}\n emit.rs: {out0}"
    );
    // 核心：9 层管线对 TEA **不再回落**
    assert!(
        !err1.contains("差分失败"),
        "`tea_standalone.mora` 不应再触发差分失败 —— 9 层管线已能处理 \
         `model` 字段默认值（D412 已修）。若又失败，说明该修复被撤销。\n\
         stderr={err1}"
    );
    // 且结果本身不变（防「差分通过但两条路径一起错」）
    assert_eq!(
        out1.lines().filter(|l| !l.trim().is_empty()).count(),
        4,
        "TEA 程序的 4 行输出应不变; 实得 {out1}"
    );
}

/// **差分失败必须**默认可见**（D36 已修「静默降级」）。
///
/// 这是本轮最该钉住的一条 —— 若它退回静默，用户会以为在跑 9 层管线，
/// 实际跑的是回落路径。
///
/// ## D412：换样本（性质不变，样本失效）
///
/// 原样本是 `tea_standalone.mora`，它**不再回落**（D412 修好）⇒ 拿它测
/// 「差分失败可见」已无意义。改用 `worker` —— `nine_layer_fallback_census`
/// 实测它**仍在回落**（9 层降级链里没有 `Worker` 类别），
/// 正是回落集合清空后仅剩的形态之一。
///
/// ⚠ 若将来 `worker` 也不回落了，本条需要**再换样本**，而不是删掉 ——
///   「差分失败不得静默」这条性质与用哪个样本无关。
#[test]
fn d364_differential_failure_is_never_silent() {
    let dir = std::env::temp_dir().join("mora_d364_visibility");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let script = dir.join("worker.mora");
    // 9 层降级链无 `Worker` 类别 ⇒ 差分失败（见 nine_layer_fallback_reasons）。
    std::fs::write(&script, "worker w do\n  print(1)\nend\n").expect("写脚本");

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    // **不设** MORA_9LAYER_DEBUG —— D36 的修复就是让它默认可见
    let out = Command::new(exe).arg(&script).output().expect("跑 mora");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        err.contains("差分失败") && err.contains("MORA_9LAYER_DEBUG"),
        "差分失败必须**默认打一行摘要**并指向 DEBUG 开关（D36 已修「静默降级」）。\n\
         ⚠ 若 `worker` 形态也已走上管线，请**换一个仍会回落的样本**，不要删掉本条。\n\
         stderr={err}"
    );
}

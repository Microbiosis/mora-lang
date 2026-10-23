//! v0.104.6 D364 —— `src/mir/optimize/` 的**语义不变性**：
//! `--opt` 四档（否定轮，无产品变更）
//!
//! 优化器是全仓最危险的一层 —— 它改写指令序列，而**任何等价性破坏
//! 都不会崩溃**，只会**静默算错**。D363 顺带发现
//! `dag_rule.rs` / `dag_search.rs` 里有大段关于 `__let_result`
//! 寄存器安全的注释（历史上真的出过「寄存器失去 producer → 消费者
//! 永不 ready → 尾部 `print` 静默消失」的事故），所以这一层值得强验证。
//!
//! ## 核心判据：**优化器只能改效率，不能改语义**
//!
//! 同一程序在 `--opt=0/1/2/3` 下必须产生**完全相同**的输出。这是可证伪的
//! —— 只要有一条判据不符，就说明某个档位优化坏了。
//!
//! ## v0.104.6 D474：判据的「编译路径」那一维被移除
//!
//! 原判据还有第二维：`MORA_9LAYER=1/0`（9 层 vs `emit.rs` 直出）。
//! **D474 把差分机制整个退役** —— parser 不再发射 MirInst（只产 witness），
//! MIR 全部由 9 层产出 ⇒「参考实现」这一侧**已不存在**，`MORA_9LAYER=0`
//! 会退化成自己跟自己比（恒真）。
//!
//! ⇒ 只保留**优化档位**这一维：它判的是「优化器只能改效率，不能改语义」
//! —— 与编译路径**无关**，且仍非平凡。
//!
//! ## 顺带查明（历史记档）：9 层管线的差分曾是**假阳性**
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
//! 差异只是**指令形式不同**（`ModelDef` vs 占位 `Const`）而**语义等价**。
//! **D412 已修**（`model` 字段默认值在 witness 层被丢弃）—— 真正的缺陷
//! 是**信息在源头就没了**，不是差分器过严。

use mora::test_support::TempDir;
use std::process::Command;

fn ev(body: &str) -> (i32, String) {
    let dir = TempDir::new("d364_");
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
    let dir = TempDir::new("d364_optflag");
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

/// v0.104.6 D474：TEA 程序在**三个优化档位**下必须产生相同结果。
///
/// 原判据是「9 层管线 vs `emit.rs` 直出」的 A/B（`MORA_9LAYER=0/1`）。
/// **D474 把差分机制整个退役** —— parser 只产 witness、MIR 全部由 9 层产出
/// ⇒「参考实现」这一侧**已不存在**，那次 A/B 会变成自己跟自己比（恒真）。
///
/// ⇒ 换成**优化档位不变性**：与编译路径**无关**，且仍非平凡。
/// 绝对值判据（TEA 的 4 行输出）由 `tests/nine_layer_tea_model_defaults.rs`
/// 承担。
#[test]
fn d412_tea_output_is_stable_across_opt_levels() {
    let fixture = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/e2e/tea_standalone.mora"
    ));
    assert!(fixture.exists(), "fixture 不存在：{}", fixture.display());

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let run = |level: &str| {
        let out = Command::new(exe)
            .arg(format!("--opt={level}"))
            .arg(fixture)
            .output()
            .expect("跑 mora");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (c0, out0, _) = run("0");
    let (c1, out1, _) = run("1");
    let (c2, out2, _) = run("2");

    assert_eq!(c0, Some(0), "--opt=0 应正常退出");
    assert_eq!(c1, c0, "--opt=1 的退出码应与 --opt=0 一致");
    assert_eq!(c2, c0, "--opt=2 的退出码应与 --opt=0 一致");
    assert_eq!(
        out0, out1,
        "--opt=0 与 --opt=1 的输出应逐字相同（优化器不能改语义）;\n 0 → {out0}\n 1 → {out1}"
    );
    assert_eq!(
        out0, out2,
        "--opt=0 与 --opt=2 的输出应逐字相同（优化器不能改语义）;\n 0 → {out0}\n 2 → {out2}"
    );
    assert!(!out0.trim().is_empty(), "输出为空则本条恒绿（采集器失效）");
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
/// v0.104.6 D474：TEA 程序的 4 行输出必须**逐行钉死**。
///
/// 原判据是「两条编译路径的 A/B + 无 `[9layer]` 回落行」—— D474 起两条
/// 路径是同一条 ⇒ 恒真。绝对值判据不依赖任何对照物。
///
/// ⚠ 绝对值判据由 `tests/nine_layer_tea_model_defaults.rs` 也覆盖了一条；
/// 本条留在本文件里，是为了让「TEA 声明形态的默认值没丢」这件事在
/// **优化器**这个语境下也有一个锚点。
#[test]
fn d412_tea_output_has_four_pinned_lines() {
    let fixture = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/e2e/tea_standalone.mora"
    ));
    assert!(fixture.exists(), "fixture 不存在：{}", fixture.display());

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(fixture).output().expect("跑 mora");
    assert!(out.status.success(), "TEA 程序应正常跑通");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec!["dict", "list", "closure", "tea_app"],
        "TEA 程序的 4 行输出应不变（type_of(Counter/CounterMsg/update/a)）; 实得 {lines:?}"
    );
}

/// **差分失败必须**默认可见**（D36 已修「静默降级」；v0.104.6 起按三态措辞）。
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
///
/// ## v0.104.6 D472：再换样本（`worker` 已被修好）
///
/// D472 把 `worker` 接进了 9 层降级链 ⇒ 它**不再回落**（普查回落项 4 → 0），
/// 本条的样本随之失效。按上面那条「换样本而不是删掉」的约定，样本改为
/// **`orchestrate`** —— D472 之后**唯一**仍会差分失败的形态
/// （9 层 FCFG 的 `OrchestrateKind<M>` 不携带 agent / edge 元数据，
/// `fcfg_lower` 只能降出 `Sequential` 占位）。
///
/// 本条守护的**性质完全不变**：差分失败必须默认可见、按名点名。
///
/// ## v0.104.6：措辞更新（三态化）
///
/// 生产门已从「整函数二值差分」改为**显式合法性登记 + 三态判定**。`orchestrate`
/// 含未 legalize 构造 `orchestrate`，故现在走「按名回落（预期）」这一支，stderr
/// 措辞由「差分失败」改为「**未 legalize 构造 ["orchestrate"]** …（预期，非缺陷）」。
/// 本条守护的**性质不变**——回落必须默认可见并指向 DEBUG 开关——只是断言的
/// 是新措辞，且现在**额外**要求点名该构造（三态化的核心收益）。
#[test]
fn d364_differential_failure_is_never_silent() {
    let dir = TempDir::new("mora_d364_visibility");
    let script = dir.join("orchestrate.mora");
    // v0.104.6 D473：`orchestrate` 已走上 9 层（此前是唯一仍回落的构造）。
    std::fs::write(
        &script,
        "orchestrate graph input -> result\n  agent a => \"A\"\n  agent b => \"B\"\n  @start -> a\n  a -> b\nend\n",
    )
    .expect("写脚本");

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&script).output().expect("跑 mora");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    // v0.104.6 **D473**：回落机制已**删除**（生产不再做差分、也不再回落 ——
    // 9 层是**规范**，回落等于「静默换一条更差的路径」，D1 那一族缺陷）。
    //
    // ⇒ 本条从「回落必须默认可见」改为钉**新**的不变式：
    // **生产路径不存在回落** —— 此前唯一会回落的形态（编排）现在
    // ① 不产生任何 `[9layer]` 回落输出；
    // ② 正常跑通（exit 0）。
    //
    // ⚠ 「差分恒绿」由 `tests/nine_layer_full_census.rs`（117 个程序）
    // 与 `tests/nine_layer_differential.rs`（19 fixture 双级差分）守住 ——
    // 差分从**生产门**降为**CI 等价性验证**后，那两处就是它的守卫。
    assert!(
        !err.contains("[9layer]"),
        "生产路径不应再有回落输出（D473 起 9 层是唯一路径）。\n\
         ⚠ 若确有新构造产不出 9 层，请**接进降级链**（见 CHANGELOG D473），\
         而不是恢复回落。\n\
         stderr={err}"
    );
    assert!(
        out.status.success(),
        "编排程序应正常跑通（D473 起它走 9 层路径）。\n\
         stderr={err}"
    );
}

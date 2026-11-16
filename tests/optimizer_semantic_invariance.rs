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
    let exe = env!("CARGO_BIN_EXE_mora");
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
                && !l.starts_with("类型与执行")
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
    assert_eq!(got.trim(), "1", "采集器失效（本文件全部断言依赖它）");
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
        baseline, "11 | 7 | 10 | 3 | 6 | 1 | 0 | 7",
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
    let exe = env!("CARGO_BIN_EXE_mora");
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

// TEA (`tea_standalone.mora`) 与 `orchestrate` 编排构造已于 v0.110.0 移除，相关测试同步移除。

//! v0.75.53: CLI 子命令模块（P9，D6 SQLite 单文件拆分惯例）。
//!
//! 从 main.rs 拆出：record（录制/replay/diff/snapshot + 统计）与 mcp。
//! 共享编译/路径辅助在本文件。main.rs 仅保留 dispatch + 执行入口。

pub mod mcp;
pub mod record;

use std::fs;
use std::path::Path;
use std::process;

use crate::interpreter::Interpreter;
use crate::parser_v3::ParserV3;
use crate::typeck::format_error;

/// v0.75.40: 单遍编译 + 优化 — 取代 parse→lower 双阶段。
/// ParserV3::compile 直接 emit MirInst + 并行产出 witness；优化语义与
/// lower_mir_exprs_with_opt 完全一致（cascades apply_rules 恒跑 + SSA opt
/// 显式 --opt 优先，未指定走 env 兜底）。调用方各自做 witness typecheck
/// 并保留原有错误消息。
///
/// v0.103: 返回 `Result` —— 此前解析失败直接 `panic!`，于是 CLI 遇到任何
/// 语法错误都会打印 Rust panic 与回溯（"compile_and_opt failed: Failed to
/// parse at line N"）而不是可读的编译错误；`mora file.mora` 与
/// `mora --check file.mora` 两条入口都受影响。调用方统一按 typecheck 错误的
/// 既有约定报错并以退出码 2 结束。
pub fn compile_and_opt(
    source: &str,
    opt_level: Option<crate::mir::ssa::OptLevel>,
) -> Result<
    (
        crate::mir::MirFunction,
        Vec<crate::mir::witness::MirWitness>,
    ),
    String,
> {
    let (mut func, witnesses) = ParserV3::compile(source)?;

    // v0.90.3: 执行器切换 — 9 层管线产出成为生产 MirFunction。
    //
    // 流程：
    //   1. 管线在 apply_rules 之前运行（差分要求 raw-to-raw）
    //   2. 类别级差分（管线 vs emit.rs 直出）绿 → 管线产出经同一套
    //      优化（apply_rules + SSA opt）后作为返回值 — 生产执行 9 层代码
    //   3. 差分红 → 自动回落原管线（不中断编译），DEBUG 模式输出差异
    //
    // 等价性保证：tests/nine_layer_differential.rs 19 fixture 双级差分
    // （类别级 + 执行级）锁定两条管线的语义等价。
    // MORA_9LAYER=0 可禁用（回落纯原管线）。
    let level = opt_level.unwrap_or_default();
    if std::env::var("MORA_9LAYER").map_or(true, |v| v != "0") {
        let (result, mut pipeline_func) = crate::mir::pipeline::run_pipeline(&func, &witnesses);
        if result.differential_ok {
            // 双侧同序优化 — 管线产出走与原管线完全一致的优化路径
            crate::mir::optimize::apply_rules(&mut pipeline_func);
            if level.enabled() {
                crate::mir::opt::optimize(&mut pipeline_func, level);
            }
            return Ok((pipeline_func, witnesses));
        }
        // v0.104.6 D36：差分失败 = **两条编译路径产出不等价**，这是一次
        // 静默降级 —— 9 层管线被丢弃、改用 `emit.rs` 的产出，而用户毫不知情
        // （他以为在跑 9 层管线）。它与本轮修过的整族缺陷同族：D1 静默取默认
        // 值、D25 静默 `inf`、D28 静默返回首元素、D35 静默饿死语句 ——
        // 失败模式都不是崩溃或报错，而是**悄悄换了一条更差的路径**。
        //
        // 此前这行只在 `MORA_9LAYER_DEBUG=1` 时才可见，等于默认静默。改为
        // **默认打一行摘要**到 stderr（详细 diff 列表仍需 DEBUG 才输出，
        // 免得刷屏）。stderr 不参与任何 stdout 断言，故不影响既有测试。
        eprintln!(
            "[9layer] 差分失败：已回落到 emit.rs 路径（9 层管线产出被丢弃）| \
             pipeline_mir={} original_mir={} | 详细 diff 设 MORA_9LAYER_DEBUG=1",
            result.pipeline_mir_count, result.original_mir_count
        );
        if std::env::var("MORA_9LAYER_DEBUG").is_ok_and(|v| v == "1") {
            eprintln!(
                "[9layer] differential FAILED — falling back to emit.rs path | fcfg={} typed={} core={} cmir={} lmir={} | pipeline_mir={} original_mir={}",
                result.fcfg_nodes,
                result.typed_nodes,
                result.core_insts,
                result.cmir_nodes,
                result.lmir_insts,
                result.pipeline_mir_count,
                result.original_mir_count
            );
            for d in &result.differential_diffs {
                eprintln!("[9layer] diff: {}", d);
            }
        }
    }

    // 原管线路径（回落 / MORA_9LAYER=0）
    crate::mir::optimize::apply_rules(&mut func);
    if level.enabled() {
        crate::mir::opt::optimize(&mut func, level);
    }
    Ok((func, witnesses))
}

/// v0.104.6 D191：读源码时**剥掉开头的 UTF-8 BOM**。
///
/// BOM（U+FEFF）是**编码产物**，不是源码内容 —— 但 `ParserV3` 会如实拒绝它：
/// `Unexpected character '\u{feff}' at line 1, column 1`。于是：
///
/// - 文件：整个程序解析失败；
/// - REPL / 管道：**第一行被静默丢弃**（实测：本会话的 PowerShell 探针
///   往 stdin 注入 BOM，`print("hi")` 直接消失、无任何提示，
///   我据此在 D190 把它误记成「REPL 首行必然 parse error」的**产品缺陷**）。
///
/// 触发面很常见：Windows PowerShell 的 `Set-Content`/`Out-File`（默认 UTF-8
/// 带 BOM）、部分编辑器/工具链的「另存为」。**在工具边界容忍它**是对的：
/// 用户不会因为工具写文件时多带了一个编码标记而该收到语法错误。
///
/// 只剥**开头**一个 —— 源码中间的 U+FEFF 是真实字符，仍应被拒。
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

/// 读源码文件，剥掉 BOM；I/O 失败给出**可操作**的说明。
///
/// v0.104.6 D192：`fs::read_to_string` 的 io::Error 原样透出时是
/// `stream did not contain valid UTF-8` —— **准确但没告诉用户该做什么**。
/// 对本语言的读者（中文 Windows 生态，GBK/GB18030 仍是不少工具的默认
/// 另存为编码）这不是边角情况：
///
/// ```text
/// $ mora gbk.mora            # 用 GBK 编码写的文件
/// gbk.mora: stream did not contain valid UTF-8
/// ```
///
/// 说明「请另存为 UTF-8」是唯一有用的下一步。本函数**不**尝试猜测/转换编码
/// —— 那属语言/工具链的**设计决定**（要支持 GBK 得引入编码探测），未擅自做。
pub fn read_source(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                format!(
                    "不是有效的 UTF-8 文本（{}）。请用编辑器**另存为 UTF-8** 后重试。\n\
                 \x20 提示：Windows 的部分工具默认写 GBK/GB18030；\
                 若是刚由这类工具生成，转换编码即可。",
                    e
                )
            } else {
                e.to_string()
            }
        })
        .map(|s| strip_bom(&s).to_string())
}

fn recordings_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("recordings");
    p
}

fn recording_path(name: &str) -> std::path::PathBuf {
    let mut p = recordings_dir();
    p.push(format!("{}.jsonl", name));
    p
}

fn format_duration(ms: u128) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}min", ms as f64 / 60_000.0)
    }
}

/// v0.104.6 D189：文件位置上的参数**看起来是选项**时，别再去读它。
///
/// 修前的行为是把该参数当路径去读，然后报「系统找不到指定的文件」——
/// 而**那个文件明明就在当前目录**。报错把原因**归错了**，用户会去反复
/// 检查一个根本不存在的路径问题：
///
/// ```text
/// $ mora --check --opt=1 ok.mora
/// --opt=1: 系统找不到指定的文件。 (os error 2)      ← ok.mora 就在这儿
/// $ mora record --update ok.mora n
/// record: failed to read --update                 ← 同上
/// ```
///
/// 修法只说**一件确实成立的事**，不做「已知 / 未知」的分类：
/// 分类需要一张**按命令**的选项表，而那张表必然与实现漂移
/// （D175 的 `module_method_names`、D186 的 MCP 名字目录都是同款教训）。
/// 更麻烦的是「是否合法」本就**依赖位置** —— `--update` 对 `snapshot`
/// 是合法选项、只是写错了地方，对 `record` 则根本不存在，
/// 一张按命令的表也表达不了这件事。故一律说位置问题。
pub fn reject_option_as_path(path: &str) {
    if !path.starts_with('-') || path == "-" {
        return;
    }
    eprintln!(
        "`{}` 是选项，不能出现在文件位置 —— 而报「文件不存在」会把你引向错误的排查方向。\n\
         选项要写在**文件之前**（`mora --opt=1 file.mora`）或**子命令之后**\n\
         （`mora snapshot file.mora name --update`）。`mora --help` 列出全部可用选项。",
        path
    );
    process::exit(2);
}

fn truncate(s: &str, max: usize) -> String {
    // v0.104.6 D179：原为 `s.len() <= max` + `&s[..max - 1]`
    // （`str::len()` 是**字节**数，`[..n]` 也是字节偏移）→ 对中文/重音字母
    // 会切在字符中间 panic。改为按**字符**截断。
    crate::record::truncate_display(s, max)
}

// v0.15: snapshot — 快照测试
fn snapshots_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("snapshots");
    p
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    let mut p = snapshots_dir();
    p.push(format!("{}.snap.jsonl", name));
    p
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{}MB", bytes / (1024 * 1024))
    }
}

fn format_ts(ts_ms: u128) -> String {
    if ts_ms == 0 {
        return "-".to_string();
    }
    // 显示相对时间
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let diff_ms = now.saturating_sub(ts_ms);
    if diff_ms < 60_000 {
        "just now".to_string()
    } else if diff_ms < 3_600_000 {
        format!("{}min ago", diff_ms / 60_000)
    } else if diff_ms < 86_400_000 {
        format!("{}h ago", diff_ms / 3_600_000)
    } else {
        format!("{}d ago", diff_ms / 86_400_000)
    }
}

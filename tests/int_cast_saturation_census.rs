//! v0.104.6 D407 —— 「`as` 整数转换**饱和**」缺陷类的**全称普查**（否定轮）
//!
//! Rust 的 `as` 转换在越界时**不 panic**，而是给出一个**看似合法的错值**：
//!
//! | 转换 | 越界行为 | 方向 |
//! |---|---|---|
//! | 浮点 → 整数（`f64 as usize`） | **饱和** | `-1.0` ⇒ `0` |
//! | 整数 → 无符号（`i64 as u64`） | **回绕** | `-1i64` ⇒ `u64::MAX` |
//!
//! ⇒ 这是一类**静默错值**来源，且两种方向**方向相反**、危害各异。
//! 本仓已逐个修过多处，故本轮做**普查**而不是再修一处。
//!
//! ## 普查结论（`src/interpreter/builtins/`，21 文件 / **30 处**整数转换）
//!
//! | 类别 | 处数 | 判定 |
//! |---|---|---|
//! | **已加守卫**（走 D246 收口 `value_as_usize`，负数报错） | — | ✅ `ai.retry` 的 `backoff_ms`（D246）、`exec.parallel` 的并发上限与 `timeout_ms`（D285，各带修前实测）、`agent.create` 的 `max_steps`（D283）、`ccr.marker` 的 `Int` 匹配（D150） |
//! | **有意保留**（明文产品契约） | 1 | ⚠ `ccr.marker` 的 size —— **D339 明确决定「不修、只钉现状 + 报告」**（D404 曾误改后回退） |
//! | **安全方向**（`usize→f64` / `char→u32` / 计数 `u64→i64`） | 其余 | ✅ 源类型已是**无符号/非负**，转换不可能饱和 |
//!
//! ⇒ **零可修项**（否定轮）。唯一未设防的那处是**有意**的，且有判据钉住。
//!
//! ## 本文件的价值是**前瞻**：新增转换会让 `as` 计数超上界而变红，
//! 迫使作者先判断「这处是否需要守卫」，而不是默默加一条饱和转换。
//!
//! ⚠ 计数**剥掉注释行**后才统计 —— 否则源码里大量解释「为什么不能饱和」
//! 的注释会被算进去（与 D388/D394/D395/D403 同族的坑）。

use std::collections::BTreeMap;
use std::path::Path;

/// 整数转换总数落在 **[20, 30]**。
///
/// ⚠ 区间而非「≤ 30」：**只有上界时，扫描器坏掉（返回 0）会静默通过**。
/// 下界 20 保证扫描器**还活着**，上界 30 挡住新增。
///
/// 增减都是**信号**：新增要判断是否需守卫；删除说明某处被收口了。
const CAST_RANGE: std::ops::RangeInclusive<usize> = 20..=30;

/// 剥掉整行注释与行尾注释。
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("//") {
                return "";
            }
            l.split("//").next().unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 数一处文本里的 `as <整型>` 出现次数（词边界，无正则依赖）。
fn count_int_casts(src: &str) -> usize {
    const TYPES: [&str; 5] = ["usize", "u32", "u64", "i64", "i32"];
    let bytes = src.as_bytes();
    let mut n = 0usize;
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        // 找 "as" 前必须是空白或行首，后跟空白
        if bytes[i] == b'a' && bytes[i + 1] == b's' && i > 0 && bytes[i - 1].is_ascii_whitespace() {
            let mut j = i + 2;
            if j < bytes.len() && bytes[j].is_ascii_whitespace() {
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j > start {
                    let word = &src[start..j];
                    if TYPES.contains(&word) {
                        n += 1;
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    n
}

fn builtin_files() -> Vec<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins");
    let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("读 builtins 目录")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("rs")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n != "mod.rs")
        })
        .collect();
    v.sort();
    v
}

// ── ① 计数普查（前瞻护栏） ──

/// **整数转换总数必须落在区间内。**
///
/// - **下界**：扫描器坏掉（返回 0）⇒ 红（否则「≤ 上界」会静默通过）；
/// - **上界**：新增一处 `as <int>` ⇒ 红，逼作者先判断它会不会饱和。
#[test]
fn d407_int_cast_count_in_range() {
    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for p in builtin_files() {
        let src = std::fs::read_to_string(&p).expect("读源码");
        let n = count_int_casts(&code_only(&src));
        if n > 0 {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            per_file.insert(name, n);
            total += n;
        }
    }
    assert!(
        CAST_RANGE.contains(&total),
        "脚本可达层的 `as <整型>` 转换共 {total} 处，不在预期区间 {CAST_RANGE:?}。\
         分布: {per_file:?}\n\
         · 落在**下界以下** ⇒ 扫描器可能坏了（`code_only` / `count_int_casts`）\n\
         · 落在**上界以上** ⇒ 新增了转换，**先判断它会不会饱和**：\n\
         \x20 · 源若是 f64 ⇒ 负数**饱和成 0**（`-1.0 as usize == 0`）\n\
         \x20 · 源若是有符号整数 ⇒ 负数**回绕**（`-1i64 as u64 == u64::MAX`）\n\
         \x20 · 用户可传数值时走 `flow::value_as_usize` 收口（D246），\
         负数/NaN/±inf 一律 `None` ⇒ 调用方报错，**不替它猜**"
    );
}

// ── ② 已加守卫处不得回退 ──

/// **D285 的两处守卫**（`exec.parallel` 的并发上限与 `timeout_ms`）仍走收口。
///
/// 这两处的修前后果都极重（D285 有实测）：
/// `Float(-1.0)` ⇒ 超时**立刻杀进程**；`Int(-1)` ⇒ 超时**永不生效**。
#[test]
fn d407_exec_parallel_guards_remain() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins/exec.rs"),
    )
    .expect("读 exec.rs");
    let code = code_only(&src);
    assert_eq!(
        code.matches("value_as_usize").count(),
        2,
        "`exec.parallel` 应恰有 2 处走 `value_as_usize`（并发上限 + timeout_ms）; \
         实得 {} 处。回退会让 `Int(-1)` 静默回绕成 u64::MAX ⇒ 超时形同虚设。",
        code.matches("value_as_usize").count()
    );
}

/// **`ai.retry` 的 `backoff_ms`** 仍走收口（D246）。
///
/// 修前 `Int(-1) as u64` 回绕成 `u64::MAX` ≈ 5.8 亿年
/// ⇒ 任何带重试的调用**永远等不到退避结束**（静默挂死）。
#[test]
fn d407_ai_retry_guard_remains() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins/ai.rs"),
    )
    .expect("读 ai.rs");
    let code = code_only(&src);
    assert!(
        code.contains("value_as_usize"),
        "`ai.retry` 的 backoff_ms 应走 `value_as_usize` 收口（D246）"
    );
    assert!(
        !code.contains("attempts_n as u64"),
        "`ai.retry` 出现了裸 `as u64` —— 守卫可能被回退"
    );
}

// ── ③ 唯一的有意例外：钉住它、且不许它悄悄变多 ──

/// **`ccr.marker` 的 size 饱和是 D339 的有意决定**，只此一处。
///
/// D404 曾把它改成报错，**随后回退** —— 因为 D339 明确记录
/// 「改它属产品契约决定」。
///
/// 本条双向钉住：它**必须在**（决定仍生效），
/// 且 builtins 层**不得再出现第二处**同形态的未设防饱和点
/// —— 而「第二处」正是 `d407_int_cast_count_under_ceiling` 负责的。
#[test]
fn d407_ccr_marker_is_the_only_intentional_exception() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins/ccr.rs"),
    )
    .expect("读 ccr.rs");
    let code = code_only(&src);
    assert!(
        code.contains(".map(|n| n as usize)"),
        "`ccr.marker` 的 size 已不再是 `as usize` 饱和 —— \
         若是有意变更（改成报错），必须同时更新 D339 的判据与 CHANGELOG"
    );
    // 判据必须还在（钉住该决定的护栏）
    let guard = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tea_max_steps_guard.rs"),
    )
    .expect("读 tests/tea_max_steps_guard.rs");
    assert!(
        guard.contains("d339_ccr_marker_negative_size_still_becomes_zero_for_both_types"),
        "D339 的钉住判据不见了 —— 该产品契约决定会失去护栏"
    );
}

// ── ④ 端到端：D285 的守卫确实生效（行为钉） ──

/// **`exec.parallel` 传负数超时必须报错**（而非静默饱和/回绕）。
#[test]
fn d407_exec_parallel_negative_timeout_errors() {
    let dir = std::env::temp_dir().join("mora_d407_exec");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let script = dir.join("p.mora");
    std::fs::write(&script, "print(exec.parallel([\"echo hi\"], 1, -1))\n").expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = std::process::Command::new(exe)
        .arg(&script)
        .output()
        .expect("跑 mora");
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert_ne!(
        out.status.code(),
        Some(0),
        "负数 timeout 应报错（D285：修前 `Int(-1)` 回绕成 u64::MAX ⇒ 超时永不生效）; out={s}"
    );
    assert!(s.contains("non-negative"), "错误应说明「非负」; out={s}");
}

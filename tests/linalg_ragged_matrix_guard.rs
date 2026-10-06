//! v0.104.6 D330 —— `linalg.matmul` / `linalg.transpose` 遇**参差矩阵**
//! （各行宽度不一致）**panic 或静默丢数据**（已修）
//!
//! ## 实测（修前）—— 3 处 panic（exit 101）+ 2 处静默丢数据
//!
//! ```text
//! linalg.transpose([[1,2],[3]])              → exit 101 panicked at linalg.rs:186
//!                                                index out of bounds: len 1, index 1
//! linalg.matmul([[1,2],[3]],[[1],[2]])       → exit 101 panicked at linalg.rs:163
//! linalg.matmul([[1,2,3],[4,5]],[[1],[2],[3]]) → exit 101 panicked at linalg.rs:163
//! linalg.transpose([[1,2,3],[4,5]])          → exit 101 panicked at linalg.rs:186
//! linalg.transpose([[1],[2,3]])              → exit 0  **[[1.0, 2.0]]** ← 第 2 行第 2 个元素消失
//! linalg.matmul([[1,2],[3,4,5]],[[1,0],[0,1]]) → exit 0 **[[1.0,2.0],[3.0,4.0]]** ← 第 3 列消失
//! ```
//!
//! **不崩的那两条更糟**：结果形状与用户写下的矩阵**不同**，
//! 却长得完全合法（行数对、列数是整数），没有任何办法从返回值发现数据丢了。
//!
//! ## 缺陷：D142 的守卫建立在一个**类型系统不保证**的前提上
//!
//! D142（`tests/linalg_dimension_checks.rs`）在本层加了「A 行数 == B 行数」
//! 的守卫，方向完全正确。但它只看 `a[0].len()` 与 `b[0].len()`
//! —— 也就是**第一行**的宽度，**假设矩阵是规整的**。
//!
//! 而 `list<list<number>>` **不表达**「每行等宽」：
//! ```text
//! [[1,2],[3]]    的类型 ≡ [[1,2],[3,4]]    的类型 ≡ list<list<Float>>
//! ```
//! 三者类型**完全相同**。所以「参差」这个错误在类型层、parser 层、
//! D142 守卫层**全都无法被看见**，一路穿到算术层的 `a[i][k]` 才炸。
//!
//! D142 判据的对照组**全部用规整矩阵**（`[[1,2],[3,4]]`、
//! `[[1,2,3],[4,5,6]]`），**零条覆盖参差** ⇒ 这是缺口，不是重复劳动。
//!
//! ## 为什么是缺陷（两条依据）
//!
//! ① **panic 直接杀进程** —— exit 101、无 `MoraError`、无诊断行。
//!    本项目所有其它维度问题（D142 的 `dot`/`cross`/`matmul`）都已走
//!    干净报错；参差是同族里**唯一**还在崩的。
//! ② **静默丢数据 = 静默改变数据形状** —— `docs/mora-spec.md:993-994`
//!    的签名是 `list<list> -> list<list>`，承诺**返回一个矩阵**；
//!    而参差输入的「结果」根本不是调用方写下的那个矩阵。
//!    这与 D325 `reshape()` 静默丢元素（`while flat.len() < total`
//!    只增长不收缩）是**同一类**已判定的缺陷。
//!
//! ## 修法：守卫加在**两个函数共用的唯一入口** `expect_f64_matrix`
//!
//! `matmul` 与 `transpose` 都只通过它取矩阵，故守卫加一次即同时覆盖，
//! 且 `ctx` 参数天然带调用点名（`linalg.matmul` / `linalg.transpose`），
//! 错误能直接告诉用户是哪个函数出的问题。
//!
//! 措辞用「维度不匹配」与 D142 **保持一致**（同一族的同一类问题），
//! 并额外给出**期望宽度 / 实际宽度 / 第几行**三个可操作信息。
//!
//! ## 不回归的四条既有语义（D142 / 本轮对照组）
//!
//! - 规整矩阵的 `matmul` / `transpose` 结果**逐字不变**；
//! - `[[]]` / `[[],[]]` 这类**全零宽**的「退化矩阵」仍返回 `[]`（宽度一致 = 0）；
//! - `matmul([[]],[[1]])` 仍报 D142 的「A 是 1×0，B 有 1 行」（新守卫在
//!   它之前不触发，因为 `[[]]` 只有一行、无从比较）；
//! - D142 钉住的 `matmul([], [])` → `[]` 空输入宽松路径**不变**。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// **每进程唯一序号** —— 防并行测试互相抢同一临时目录。
///
/// D321 已记过一次同族坑（tag 含 `/` 变嵌套路径、`remove_dir_all` 只删叶子）。
/// 本条是它的**第二形态**：目录名不冲突，但 `remove_dir_all` / `create_dir_all`
/// 与 `Command::output()` **没有同步** —— 五个测试函数并行时，A 线程可能在
/// B 线程 `output()` 期间把 B 的工作目录删掉，于是 B 拿到**空输出**。
///
/// 症状极具误导性：`exit == 1` **正确**（进程确实跑了、确实报错了），
/// 只有**文本**是空的 ⇒ 看起来像「守卫没生效」，实际是**装置自相残杀**。
static SEQ: AtomicU64 = AtomicU64::new(0);

fn uniq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d330_ragged_{}_{}", uniq(), slug(tag)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    // ⚠ **不要用 `find(|第一行)`** —— 诊断行前面可能有包装行 / 空行，
    // 取第一行会漏掉真正的错误文本。这里**保留全部实质行**交给断言 `contains`。
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
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn ev(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **精确取值**用：只取 stdout 的实质行（诊断在 stderr 上，不会混进来），
/// 供「结果逐字不变」类对照组比对。绝不与 [`run`] 的诊断视图混用 ——
/// 后者会把 stdout 与 stderr 拼在一起，破坏逐字比对。
fn ev_stdout(e: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d330_out_{}_{}", uniq(), slug(e)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, format!("print({e})\n")).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
        })
        .unwrap_or("<empty>")
        .to_string();
    (out.status.code().unwrap_or(-1), first)
}

/// **主断言**：参差矩阵必须**干净报错**，既不得 panic（exit 101）
/// 也不得静默丢数据。
///
/// 五种形态全覆盖，缺一不可：
/// - `transpose` 短行（`[[1,2],[3]]`）—— 修前 panic；
/// - `transpose` 长行（`[[1],[2,3]]`）—— 修前**静默丢数据**；
/// - `matmul` 短行 × 匹配列数 —— 修前 panic；
/// - `matmul` 长行 × 单位矩阵 —— 修前**静默丢列**；
/// - 三行参差（`[[1,2,3],[4,5]]`）—— 修前 panic。
#[test]
fn d330_ragged_matrices_are_rejected_not_panicking_or_dropping_data() {
    for e in [
        // 修前：panic（exit 101）
        "linalg.transpose([[1,2],[3]])",
        "linalg.transpose([[1,2,3],[4,5]])",
        "linalg.matmul([[1,2],[3]],[[1],[2]])",
        "linalg.matmul([[1,2,3],[4,5]],[[1],[2],[3]])",
        // 修前：静默丢数据（exit 0，长得完全合法）
        "linalg.transpose([[1],[2,3]])",
        "linalg.matmul([[1,2],[3,4,5]],[[1,0],[0,1]])",
    ] {
        let (code, got) = ev(e);
        assert_ne!(
            code, 101,
            "`{e}` 修前会 **panic**（exit 101）; 实得 exit={code} out={got}\n\
             守卫必须在算术层之前拦住参差矩阵"
        );
        assert_eq!(
            code, 1,
            "`{e}` 应干净报错（exit 1）; 实得 exit={code} out={got}\n\
             修前两条会**静默丢数据**（exit 0、形状合法、无从发现元素消失）"
        );
        assert!(
            got.contains("维度不匹配"),
            "`{e}` 应报「维度不匹配」（与 D142 同族同措辞）; 实得: {got}"
        );
        assert!(
            got.contains("宽度"),
            "`{e}` 的错误应说明是**行宽**不一致; 实得: {got}"
        );
    }
}

/// 错误信息必须**点名调用方**并给出可操作的三个数字。
///
/// `ctx` 是在共用入口 `expect_f64_matrix` 里带下来的，
/// 所以两条路径的报错都能指明是 `matmul` 还是 `transpose` 出的问题。
#[test]
fn d330_error_names_the_function_and_the_offending_row() {
    let (code, got) = ev("linalg.transpose([[1,2],[3]])");
    assert_eq!(code, 1);
    assert!(
        got.contains("linalg.transpose"),
        "错误应点名 `linalg.transpose`; 实得: {got}"
    );
    assert!(
        got.contains("第 1 行 2 列") && got.contains("第 2 行 1 列"),
        "错误应给出期望宽度与实际宽度及行号; 实得: {got}"
    );

    let (code, got) = ev("linalg.matmul([[1,2,3],[4,5]],[[1],[2],[3]])");
    assert_eq!(code, 1);
    assert!(
        got.contains("linalg.matmul"),
        "错误应点名 `linalg.matmul`; 实得: {got}"
    );
    assert!(
        got.contains("第 1 行 3 列") && got.contains("第 2 行 2 列"),
        "错误应给出期望宽度与实际宽度及行号; 实得: {got}"
    );
}

/// **对照组 1**：规整矩阵的结果**逐字不变** —— 本条不能把 linalg 弄坏。
#[test]
fn d330_regular_matrices_still_work() {
    for (e, want) in [
        (
            "linalg.matmul([[1,2],[3,4]],[[1,0],[0,1]])",
            "[[1.0, 2.0], [3.0, 4.0]]",
        ),
        (
            "linalg.matmul([[1,2],[3,4]],[[5,6],[7,8]])",
            "[[19.0, 22.0], [43.0, 50.0]]",
        ),
        (
            "linalg.transpose([[1,2,3],[4,5,6]])",
            "[[1.0, 4.0], [2.0, 5.0], [3.0, 6.0]]",
        ),
        // 行向量 × 列向量
        ("linalg.matmul([[1,2,3]],[[1],[2],[3]])", "[[14.0]]"),
        // 列向量 × 行向量
        (
            "linalg.matmul([[1],[2],[3]],[[1,2,3]])",
            "[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [3.0, 6.0, 9.0]]",
        ),
        // 3×1 与 1×3
        ("linalg.transpose([[1],[2],[3]])", "[[1.0, 2.0, 3.0]]"),
    ] {
        let (code, got) = ev_stdout(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`{e}` 应得 {want}; 实得 {got}");
    }
}

/// **对照组 2**：**全零宽**的「退化矩阵」不被误伤。
///
/// `[[]]` / `[[],[]]` 的行宽**都是 0**，符合「一致」⇒ 不该报错。
/// 它们同时是 D142 之前就有的合法输入（返回 `[]`）。
#[test]
fn d330_zero_width_rows_are_not_rejected() {
    for (e, want) in [
        ("linalg.transpose([[]])", "[]"),
        ("linalg.transpose([[],[]])", "[]"),
        ("linalg.transpose([])", "[]"),
        ("linalg.matmul([],[])", "[]"),
        ("linalg.matmul([],[[1,2]])", "[]"),
        ("linalg.matmul([[1,2]],[])", "[]"),
    ] {
        let (code, got) = ev_stdout(e);
        assert_eq!(
            code, 0,
            "`{e}` 应成功（D142 钉住的空输入宽松路径）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{e}` 应得 {want}; 实得 {got}");
    }
}

/// **对照组 3**：D142 的「A 行数 ≠ B 行数」诊断**不被新守卫抢走**。
///
/// `matmul([[]],[[1]])`：A 是 1×0、B 有 1 行。新守卫不触发（`[[]]` 只有
/// 一行，无从比较行宽），因此仍应得到 D142 的那条消息。
#[test]
fn d330_d142_row_count_message_still_owns_that_case() {
    let (code, got) = ev("linalg.matmul([[]],[[1]])");
    assert_eq!(code, 1, "应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("A 是 1×0") && got.contains("1 行"),
        "该用例属 D142 的「A 行数 ≠ B 行数」，应仍由它诊断; 实得: {got}"
    );
}

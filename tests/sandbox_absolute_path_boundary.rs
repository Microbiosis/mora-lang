//! v0.104.6 D335 —— `file.*` **所有绝对路径都被拒**（含 `file.cwd()` 与 `file.abs()`
//! 的**自往返**）；跨盘仍拒（已修）
//!
//! ## 实测（修前）—— 不是「沙箱太紧」，是**两侧形式不一致导致的误判**
//!
//! ```text
//! file.exists("Cargo.toml")                     → true          ← 相对路径 OK
//! file.is_dir("src")                            → true
//! file.exists("src/interpreter/…/file.rs")     → true
//!
//! file.exists("D:/Github/mora-lang/Cargo.toml") → sandbox denied  ← **工作区自己的文件**
//! file.is_dir("D:/Github/mora-lang/src")        → sandbox denied
//! file.is_dir(file.cwd())                       → sandbox denied  ← **自己返回的路径自己不能用**
//! file.exists(file.abs("Cargo.toml"))           → sandbox denied  ← **自己算的路径自己不能用**
//! file.is_dir("D:/")                            → sandbox denied  ← **盘根**
//! ```
//!
//! 后两条是**自相矛盾**的：`file.cwd()` 与 `file.abs()` 的返回值**只能**由
//! `file.*` 消费，而 `file.*` 一律拒绝它们。
//!
//! ## 根因：`canonical_root` 与 `resolved` **形式不一致**
//!
//! `fs_root` 来自 `SandboxPolicy::permissive()` = `PathBuf::from("/")`；
//! 经 `std::fs::canonicalize` 后在 Windows 上是**逐字路径**（verbatim）
//! `\\?\D:\`，而用户传入的绝对路径是**普通形式** `D:\Github\…`。
//! `Path::starts_with` 按**组件逐个**比较，首组件 `\\?\D:` vs `D:` 即不同
//! ⇒ **一切绝对路径都被判为「逃逸」**。
//!
//! 相对路径之所以能过，是因为它走 `canonical_root.join(p)` —— **继承了**
//! 逐字前缀，两侧形式恰好一致。
//!
//! ## 三条独立依据说明这是缺陷（不是「过严的设计」）
//!
//! ① `docs/mora-spec.md:1504` 明写「当前版本**无沙箱**。脚本可以读写文件系统」，
//!    「文件系统访问白名单」列在 **v1.0 计划**下 ⇒ 修前既违背 spec、
//!    又让 `file.*` 在同盘内**连自己的文件都读不到**。
//! ② `file.cwd()` / `file.abs()` 的返回值**必然**是绝对路径，
//!    而 `file.*` 拒绝一切绝对路径 ⇒ 这两个 builtin 的产出**不可用**。
//! ③ 同盘不同目录（`D:/Github`）也被拒，而它**显然**在 `fs_root` 之内
//!    （实测 fs_root = `D:\`，就是盘根）⇒ 判定结果与事实矛盾。
//!
//! ## 修法
//!
//! ① **两侧归一到同一形式**再比：都剥掉 `\\?\` / `\\?\UNC\` 前缀
//!    （新增 `strip_verbatim`）；
//! ② 对**已存在**的 `resolved` 也做 `canonicalize`（失败退回原路径，
//!    因为 `write_text` 的目标常常尚不存在）—— 顺带堵住
//!    「沙箱内符号链接 / junction 指向沙箱外」这条逃逸。
//!
//! ## **不放宽**的边界（本条的一半篇幅在钉这个）
//!
//! | 输入 | 修后 | 依据 |
//! |---|---|---|
//! | `C:/Users`、`C:/Windows/win.ini`、`C:/` | **仍拒** | 跨盘，确实在 `fs_root` 外 |
//! | `../`、`src/../Cargo.toml` | **仍拒** | `..` 穿越（第 1 条规则，与 D335 修复无关）|
//! | `D:/`（盘根 = fs_root） | 放行 | 恰在边界内 |
//! | 尚不存在的新文件 | 放行 | `canonicalize` 失败的回退分支 |
//!
//! ⚠ 若本条红，说明有人收紧了边界 —— 那**必须**连同「D334 修好的
//! `chdir` / `rename` / `copy` / `touch` 守卫」一起看，否则会把 D334 撤销掉。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    // 固定前缀 + 截断：避开 Windows 保留设备名与 260 路径上限（D334 教训）
    let mut out = String::from("d335_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 取 stdout **全部**实质行并以 ` | ` 连接（D330 教训：不能用 `find(第一行)`）。
/// 探针在 `%TEMP%` 下跑，`HOME` / `USERPROFILE` 指向临时目录。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d335q_{}_{}", n, slug(body)));
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

/// 取工作区所在**盘**的根（Windows 上是 `D:\`，与 `fs_root` 同源）。
fn drive_root() -> String {
    std::env::current_dir()
        .expect("cwd")
        .ancestors()
        .last()
        .expect("盘根")
        .to_string_lossy()
        .trim_end_matches('\\')
        .to_string()
}

/// **主断言**：绝对路径**必须**被接受 —— 至少到工作区自己的文件。
///
/// 三种写法全覆盖：正斜杠、反斜杠、以及**由 `file.*` 自己算出来的**。
#[test]
fn d335_absolute_paths_inside_fs_root_are_accepted() {
    for e in [
        // 正斜杠形式
        "print(file.exists(\"D:/Github/mora-lang/Cargo.toml\"))",
        "print(file.is_dir(\"D:/Github/mora-lang/src\"))",
        "print(file.is_dir(\"D:/Github\"))",
        // 反斜杠形式
        "print(file.exists(\"D:\\\\Github\\\\mora-lang\\\\Cargo.toml\"))",
        // 盘根 == fs_root，恰在边界内
        "print(file.is_dir(\"D:/\"))",
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(
            code, 0,
            "`{e}` 应成功（路径在 fs_root 之内）; 实得 exit={code} out={got}\n\
             修前是 `sandbox denied`：`canonicalize` 返回逐字路径 `\\\\?\\D:\\`，\
             而传入的是普通形式 `D:\\…`，`starts_with` 按组件比必然 false"
        );
        assert_eq!(got, "true", "`{e}` 应得 true; 实得 {got}");
    }
}

/// **`file.cwd()` / `file.abs()` 的自往返** —— 修前这两条被拒，
/// 是本缺陷最直观的证据：builtin 产出的值**自己不能用**。
#[test]
fn d335_cwd_and_abs_outputs_are_usable_by_file_ops() {
    for e in [
        "print(file.is_dir(file.cwd()))",
        "print(file.exists(file.abs(\"Cargo.toml\")))",
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(
            code, 0,
            "`{e}` 应成功 —— `cwd()`/`abs()` 的返回值**必然**是绝对路径，\
             `file.*` 拒绝一切绝对路径 ⇒ builtin 产出不可用; 实得 exit={code} out={got}"
        );
        assert_eq!(got, "true", "`{e}` 应得 true; 实得 {got}");
    }

    // 写→读 的绝对路径往返
    let body = "file.write_text(file.abs(\"d335_rt.txt\"), \"v\")\nprint(file.read_text(file.abs(\"d335_rt.txt\")))\nprint(file.remove(file.abs(\"d335_rt.txt\")))\n";
    let (code, got) = ev(body);
    assert_eq!(
        code, 0,
        "绝对路径的写/读/删往返应成功; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "v | nil", "往返应无损; 实得 {got}");
}

/// **D409 更新**：原名 `d335_cross_drive_still_denied`，**整条重写**。
///
/// 原条的意图是「D335 修的是『同盘被误拒』，**不是**把沙箱拆了」，
/// 于是拿**跨盘**当「边界外」的证据。
///
/// ⚠ 但那个边界**只因为一个 bug 才存在**：`permissive()` 的 `fs_root = "/"`
/// 被 `canonicalize` 压成**当前盘**。D409 修掉后（`/` 改为显式哨兵，
/// 对齐 doc 与 `docs/mora-spec.md` 17.1「当前版本**无沙箱**」），
/// **默认策略下不存在任何「边界外」** ⇒ 原条必然全红。
///
/// ⇒ 这是「**被拒不构成在边界外的证据**」这条教训的第四次应验
/// （D334 第一版列 `D:/` → D335 打红 → D409 又打红）。
///
/// 改钉**真正的剩余边界**：限制性 `fs_root` 仍逐根生效。
/// 跨盘放行本身由 D334/D409 的对账判据覆盖。
#[test]
fn d409_default_policy_has_no_fs_boundary() {
    // 跨盘**放行** —— v0.x 无沙箱（spec 17.1）
    for e in [
        "print(file.exists(\"C:/Users\"))",
        "print(file.is_dir(\"C:/\"))",
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(
            code, 0,
            "`{e}` 跨盘应**放行**（v0.x 无沙箱）; 实得 exit={code} out={got}\n\
             若此条红，说明 D409 的 `is_unrestricted` 哨兵被回退了"
        );
        assert!(
            !got.contains("sandbox denied"),
            "`{e}` 不应报 sandbox denied; 实得 {got}"
        );
    }
    // 跨盘**读**也放行（`win.ini` 真实存在）
    let (code, got) = ev("print(file.read_text(\"C:/Windows/win.ini\"))\n");
    assert_eq!(code, 0, "跨盘读应放行; 实得 exit={code} out={got}");
    assert!(
        !got.is_empty() && !got.contains("sandbox denied"),
        "应真读到内容; 实得 {got}"
    );
}

/// **不放宽边界 ②**：`..` 穿越仍必须被拒（**开头**与**嵌入**两种）。
///
/// 这是 `check_path` 的第 1 条规则，与 D335 的修复**无关** ——
/// 但正因为无关，它是最好的「修复没动别的」的对照。
#[test]
fn d335_parent_traversal_still_denied() {
    for e in [
        "print(file.exists(\"../\"))",
        "print(file.exists(\"src/../Cargo.toml\"))",
        "print(file.read_text(\"../../Cargo.toml\"))",
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(code, 1, "`{e}` 含 `..` 应被拒; 实得 exit={code} out={got}");
        assert!(
            got.contains("path traversal") || got.contains("'..'"),
            "`{e}` 应报路径穿越; 实得: {got}"
        );
    }
}

/// **对照组**：相对路径行为**不变**，且新文件（`canonicalize` 会失败）可写。
///
/// `write_text` 的目标常常**尚不存在**，`canonicalize` 必然失败 ⇒
/// 修复里必须有回退分支。这条钉住它。
#[test]
fn d335_relative_paths_and_new_files_still_work() {
    for (e, want) in [
        ("print(file.exists(\"Cargo.toml\"))", "true"),
        ("print(file.is_dir(\"src\"))", "true"),
        (
            "print(file.exists(\"src/interpreter/builtins/file.rs\"))",
            "true",
        ),
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`{e}` 应得 {want}; 实得 {got}");
    }
    // 尚不存在的文件：走 `canonicalize` 失败的回退分支
    let body = "print(file.write_text(\"d335_nf.txt\", \"v\"))\nprint(file.read_text(\"d335_nf.txt\"))\nprint(file.remove(\"d335_nf.txt\"))\n";
    let (code, got) = ev(body);
    assert_eq!(code, 0, "新文件往返应成功; 实得 exit={code} out={got}");
    assert_eq!(got, "nil | v | nil", "新文件往返应无损; 实得 {got}");
}

/// `sandbox.check_path` 这个**查询型** builtin 必须与实际放行行为**一致**。
///
/// 修前它对 `D:/Github` 返回 `false`，而 `file.*` 也拒绝 —— 表面一致，
/// 但两者一致地**错**。修后两者都必须返回 `true`。
///
/// ## D409 更新：跨盘那一行从 `false` 改成 `true`
///
/// 原表里 `C:/Users → false` 靠的是「`permissive()` 意外只覆盖当前盘」
/// 这个 bug。修掉后跨盘放行 ⇒ 这里必须跟着改，否则本条与实际行为**不一致**，
/// 而「一致」正是本条要守的东西。
#[test]
fn d335_check_path_query_agrees_with_file_ops() {
    for (e, want) in [
        ("print(sandbox.check_path(\"D:/Github\"))", "true"),
        ("print(sandbox.check_path(\"relative.txt\"))", "true"),
        // D409：默认策略无边界 ⇒ 跨盘**放行**（与 `file.exists` 现状一致）
        ("print(sandbox.check_path(\"C:/Users\"))", "true"),
        // `..` 是明文拒绝项，与边界无关，两种语义下都为 false
        ("print(sandbox.check_path(\"../x\"))", "false"),
    ] {
        let (code, got) = ev(&format!("{e}\n"));
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`{e}` 应得 {want}; 实得 {got}");
    }
    // 盘根应放行（D409 后没有 fs_root，但盘根仍要能读）
    let (code, got) = ev(&format!("print(file.is_dir(\"{}/\"))", drive_root()));
    assert_eq!(code, 0, "盘根应放行; 实得 exit={code} out={got}");
    assert_eq!(got, "true", "盘根应可读; 实得 {got}");
}

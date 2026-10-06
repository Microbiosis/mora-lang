//! v0.104.6 D334 —— `file.*` 的 **4 个入口漏调 sandbox 守卫**（已修）
//!
//! ## ⚠⚠ 先读这段（D409 更新）—— 下面那份矩阵是**历史记录**，不是现状
//!
//! D409 修掉了 `permissive()` 的一个 bug：`fs_root = "/"` 会被
//! `canonicalize` 压成**当前盘**（`D:\`），于是「无限制」实际是
//! 「**只能访问当前盘**」。
//!
//! ⇒ 下面「实测（修前）」那张表里形如
//! `file.read_text("C:/Windows/win.ini") → sandbox denied` 的行，
//! **在 D409 之后不再成立** —— 默认策略下 `C:/…` 会被**放行**，
//! 因为 `docs/mora-spec.md` 17.1 明写「当前版本**无沙箱**。脚本可以读写文件系统」。
//!
//! 所以本文件的**行为判据已改用含 `..` 的越界路径**（`check_path` 自己的
//! 专属诊断 `path traversal`，与 OS 错误一眼可分），D334 真正要证明的
//! 东西 ——「每个带路径的入口都过守卫，而不是落到 OS」—— **一字未损**。
//!
//! ## 同一个坑，第二次
//!
//! 本文件原先在 `d334_chdir_rejects_any_outside_path` 里已经记过一次：
//! D334 第一版把 `D:/` 也列成「沙箱外」，那是**照着 bug 写的**，
//! D335 修好后被打红。本次是**同族第二次** —— 判据的有效性依赖了一个
//! 恰好存在的边界，而这个边界本身是缺陷。
//!
//! ⇒ 教训（值得单列）：**「被拒」不构成「在边界外」的证据。**
//! 判据若靠「某输入被拒」来界定边界，必须确认那个边界的**来源**；
//! 来源若是 bug，这条判据就是**照着 bug 写的**。
//!
//! ## 实测（修前）：26 入口矩阵 + 独立的 sandbox 覆盖矩阵
//!
//! 带路径的入口分两类 —— 有守卫的报 `sandbox denied`，漏守卫的报 **OS 错误**：
//!
//! ```text
//! file.read_text("C:/Windows/win.ini")   → sandbox denied ... escapes fs_root   ← 有守卫
//! file.exists("C:/Windows/win.ini")      → sandbox denied ...                    ← 有守卫
//! file.write_text("C:/x.txt", …)         → sandbox denied ...                    ← 有守卫
//! file.mkdir("C:/dir")                   → sandbox denied ...                    ← 有守卫
//! file.remove("C:/nope")                 → sandbox denied ...                    ← 有守卫
//! file.remove_all("C:/nope")             → sandbox denied ...                    ← 有守卫
//!
//! file.touch("C:/x.txt")                 → cannot create ... 拒绝访问 (os error 5)  ← **漏**
//! file.rename("C:/a.txt", "C:/b.txt")    → cannot rename ... 系统找不到指定的文件    ← **漏**
//! file.copy("C:/a.txt", "C:/b.txt")      → cannot copy ... 系统找不到指定的文件      ← **漏**
//! file.chdir("C:/")                      → **nil，exit 0，完全成功**                ← **漏**
//! ```
//!
//! ## `chdir` 为什么是**最严重**的一个
//!
//! 不是「它自己能读写沙箱外」——而是它**移动了判定的基准**：
//! `check_path` 拿相对路径与 `fs_root` 比对，而相对路径是相对
//! **当前工作目录**解析的。一次 `chdir` 到沙箱外，
//! 就让**其后所有** `file.*` 操作的相对路径都从沙箱外解析。
//!
//! 实测（修前，`%TEMP%` 是普通用户可写目录，故未受 OS 权限阻挡）：
//!
//! ```mora
//! file.chdir("C:/Users/<u>/AppData/Local/Temp")  → nil
//! file.cwd()                                        → C:\Users\<u>\AppData\Local\Temp
//! file.write_text("d334_escape2.txt", "escaped")    → nil          ← **写成功**
//! file.read_text("d334_escape2.txt")                → "escaped"    ← 读回成功
//! file.list(".")                                     → 10000 项
//! ```
//!
//! ⇒ `write_text` / `read_text` / `list` **各自都有守卫**，
//! 但守卫算出的路径已经错了 ⇒ **sandbox 被完整绕过**。
//! （该次实测在 `%TEMP%` 留下真实文件，**已清理**。）
//!
//! ⇒ 本条不是「补四个漏掉的守卫」，而是**堵住让所有守卫同时失效的入口**。
//!
//! ## 判定依据：代码**自己的注释**在承诺这件事
//!
//! `file.rs` 顶部写着「v0.36: enforce sandbox on **every** path-bearing file op」——
//! **这句话此前是假的**。同时 `docs/mora-spec.md:1162` 把 `rename` / `copy` /
//! `touch` / `chdir` 列为「完整 API」的一部分，即承诺它们**存在且可用**。
//!
//! ## 修法：四个入口各补守卫
//!
//! - `rename` / `copy`：**两个路径都要查** —— `from` 是读源、`to` 是写目标，
//!   少查任一个都能被用来把数据搬出沙箱；
//! - `touch`：会**创建**文件，是写操作；
//! - `chdir`：见上，是「让守卫失效」的元凶。
//!
//! 修后 4 个入口的诊断与其它入口**逐字一致**
//! （`sandbox denied '…': … escapes fs_root`）。
//!
//! ## 未守卫且**不应**守卫的入口
//!
//! `join` / `basename` / `dirname` / `extname` / `abs` 是**纯字符串运算**
//! —— 不触碰文件系统，守卫它们只会产生误报。`abs` 尤其要注意：它
//! **不做路径解析检查**（只拼 cwd），故 `file.abs("C:/x")` 正常返回。
//! 这两个在本文件末尾单独钉住。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    // ⚠ 两个 Windows 陷阱（D334 判据第一版连踩两个）：
    //
    // ① **保留设备名**：`basename` / `dirname` / `exists` / `remove` 这些 tag
    //    slug 化后落进 `CON` / `NUL` / `PRN` / `AUX` / `COM1`… 的形态，
    //    `create_dir_all` 报 `os error 123 文件名…语法不正确`。
    //    办法：**加固定前缀**（`d334_`）—— 加了前缀就不再是保留名。
    //    比「逐个查保留名表」稳：那张表有大小写与扩展名的坑
    //    （`NUL.txt` 同样是设备名）。
    //
    // ② **路径长度**：多行探针 body 很长，整段 slug 化后
    //    `%TEMP%\mora_d334_f_<seq>_<slug>\p.mora` 会**超 260**，
    //    同样报 `os error 123`。办法：**只取前 40 个字符**。
    //    目录名不需要可读，只需要唯一 —— 唯一性由前面的 `AtomicU64` 序号保证。
    let mut out = String::from("d334_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mora_d334_f_{}_{}", n, slug(body)));
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

// ── D409：越界探针常量 ──
//
// 公共形态：`../d334_outside_absent/<x>`。
//
// 为什么指向**父目录下确定不存在的目录**，而不是随便一个 `../x`：
// 若某个入口**漏了守卫**，OS 报的会是 `os error 2`（找不到），
// 与守卫在时的 `path traversal` 诊断**一眼可分**，
// 且**两种情况都不会在磁盘上留下任何真实文件**。
//
// ⚠ 不要把这里改回 `C:/...`：D409 之前那样写是有效的，
//   但它依赖「`permissive()` 意外地只覆盖当前盘」这个 **bug**。

const OUT_READ_TEXT: &str = "print(file.read_text(\"../d334_outside_absent/f.txt\"))\n";
const OUT_READ_BYTES: &str = "print(file.read_bytes(\"../d334_outside_absent/f.txt\"))\n";
const OUT_EXISTS: &str = "print(file.exists(\"../d334_outside_absent/f.txt\"))\n";
const OUT_IS_FILE: &str = "print(file.is_file(\"../d334_outside_absent/f.txt\"))\n";
const OUT_IS_DIR: &str = "print(file.is_dir(\"../d334_outside_absent\"))\n";
const OUT_SIZE: &str = "print(file.size(\"../d334_outside_absent/f.txt\"))\n";
const OUT_LIST: &str = "print(file.list(\"../d334_outside_absent\"))\n";
const OUT_WRITE_TEXT: &str = "print(file.write_text(\"../d334_outside_absent/w.txt\", \"x\"))\n";
const OUT_APPEND_TEXT: &str = "print(file.append_text(\"../d334_outside_absent/w.txt\", \"x\"))\n";
const OUT_WRITE_BYTES: &str = "print(file.write_bytes(\"../d334_outside_absent/w.bin\", \"00\"))\n";
const OUT_MKDIR: &str = "print(file.mkdir(\"../d334_outside_absent\"))\n";
const OUT_MKDIR_ALL: &str = "print(file.mkdir_all(\"../d334_outside_absent/deep\"))\n";
const OUT_REMOVE: &str = "print(file.remove(\"../d334_outside_absent/f.txt\"))\n";
const OUT_REMOVE_ALL: &str = "print(file.remove_all(\"../d334_outside_absent\"))\n";
const OUT_TOUCH: &str = "print(file.touch(\"../d334_outside_absent/t.txt\"))\n";
/// `rename` / `copy`：**源**越界。
const OUT_RENAME_OUT: &str =
    "print(file.rename(\"../d334_outside_absent/a.txt\", \"d334_local.txt\"))\n";
/// `rename` / `copy`：**目标**越界。
const OUT_RENAME_IN: &str =
    "print(file.rename(\"d334_local.txt\", \"../d334_outside_absent/b.txt\"))\n";
const OUT_COPY_OUT: &str =
    "print(file.copy(\"../d334_outside_absent/a.txt\", \"d334_local.txt\"))\n";
const OUT_COPY_IN: &str =
    "print(file.copy(\"d334_local.txt\", \"../d334_outside_absent/b.txt\"))\n";
const OUT_CHDIR: &str = "print(file.chdir(\"../d334_outside_absent\"))\n";

fn chdir_body(path: &str) -> String {
    format!("print(file.chdir(\"{path}\"))\n")
}

/// **主断言（行为侧）**：全部 10 个带路径的写/读/元数据入口，
/// 都必须给**同一种** `sandbox denied` 诊断。
///
/// 「同一种」是重点 —— 漏掉的入口会退化成 **OS 错误**
/// （`os error 2` / `os error 5`），那一眼就能看出差别。
///
/// ## ⚠ D409：探针从「跨盘」换成「`..`」—— 原因必须留档
///
/// 本条**第一版**用 `C:/Windows/win.ini` 当「沙箱外」。而它之所以在「外」，
/// **只因为一个 bug**：`permissive()` 的 `fs_root = "/"` 被 `canonicalize`
/// 压成**当前盘**（`D:\`）⇒ `C:/…` 才显得在外面。
///
/// D409 修掉那个 bug（`/` 改为**显式哨兵**，`permissive()` 真正「无限制」，
/// 与 `docs/mora-spec.md` 17.1「当前版本**无沙箱**」对齐）后，
/// **默认策略下根本不存在「沙箱外」** ⇒ 这些断言必然全红。
///
/// ⇒ 这**不是** D409 破坏了功能，而是这批断言**一直靠 bug 才成立**。
/// 本文件第「未钉的观察」里已记过同族事故一次（D334 第一版把 `D:/`
/// 也列成「沙箱外」，D335 修好后被打红）—— **同一个坑，第二次**。
///
/// ## 换用的探针：含 `..` 的路径
///
/// `check_path` 对 `..` 的拒绝是**它自己的专属诊断**
/// （`sandbox denied '…': … contains '..' (path traversal)`），
/// 而 OS 不会说这句话 ⇒ **它照样能区分「过了守卫」与「落到 OS」**，
/// 且在 v0.x「无沙箱」语义下**依然成立**（`..` 拒绝是明文决定，D409 未动）。
///
/// 路径故意指向**父目录下不存在的目录**（`../<不存在目录>/x`）：
/// 守卫在 ⇒ 报 `path traversal`；守卫不在 ⇒ 报 `os error 2`。
/// 两种情况**都不会留下任何真实文件**。
#[test]
fn d334_every_path_bearing_op_denies_outside_sandbox() {
    for (method, body) in [
        ("read_text", OUT_READ_TEXT),
        ("read_bytes", OUT_READ_BYTES),
        ("exists", OUT_EXISTS),
        ("is_file", OUT_IS_FILE),
        ("is_dir", OUT_IS_DIR),
        ("size", OUT_SIZE),
        ("list", OUT_LIST),
        ("write_text", OUT_WRITE_TEXT),
        ("append_text", OUT_APPEND_TEXT),
        ("write_bytes", OUT_WRITE_BYTES),
        ("mkdir", OUT_MKDIR),
        ("mkdir_all", OUT_MKDIR_ALL),
        ("remove", OUT_REMOVE),
        ("remove_all", OUT_REMOVE_ALL),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 1,
            "`file.{method}` 对越界路径应报错; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("sandbox denied"),
            "`file.{method}` 必须给 **sandbox denied**; 实得: {got}\n\
             若实得是 OS 错误（os error 2/5），说明本入口漏调 `check_path`"
        );
        assert!(
            got.contains("path traversal"),
            "`file.{method}` 的拒绝应来自 `check_path` 本身（`..` 规则）; 实得: {got}\n\
             ⚠ 若实得是 `escapes fs_root`，说明守卫在但探针已过时（见 D409 说明）"
        );
    }
}

/// **主断言（D334 新修的四个）**：`rename` / `copy` / `touch` / `chdir`
/// 现在与其它入口**逐字一致**。
///
/// 这四条在修前分别给 `os error 5` / `os error 2` / `os error 2` / **成功**。
#[test]
fn d334_d334_four_ops_now_deny_instead_of_falling_through_to_os() {
    for (method, body) in [
        ("touch", OUT_TOUCH),
        ("rename", OUT_RENAME_OUT),
        ("copy", OUT_COPY_OUT),
        ("chdir", OUT_CHDIR),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 1,
            "`file.{method}` 必须**拒绝**（修前 `chdir` 是 exit 0 完全成功）; \
             实得 exit={code} out={got}"
        );
        assert!(
            got.contains("sandbox denied") && got.contains("path traversal"),
            "`file.{method}` 的诊断应与其它入口逐字一致; 实得: {got}"
        );
    }
}

/// **`chdir` 专项**：它不能只是「拒绝一个目录」，
/// 而必须**拒绝一切越界路径**——否则换个目录就漏了。
///
/// ⚠ 「失败后 cwd 未被动过」**无法在同一进程内验证**：本运行时
/// 遇错即终止进程，后续语句不会执行（D334 判据第一版正是这样写、
/// 拿到空输出而误以为产品有问题）。故拆成两个独立进程：
/// 一个跑 `chdir` 看是否被拒，另一个单独读 `cwd`。
#[test]
fn d334_chdir_rejects_any_outside_path() {
    for (path, body) in [
        ("../d334_outside_absent", OUT_CHDIR),
        (
            "../d334_outside_absent_b",
            &chdir_body("../d334_outside_absent_b"),
        ),
        (
            "..\\d334_outside_absent_c",
            &chdir_body("..\\d334_outside_absent_c"),
        ),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 1,
            "`file.chdir(\"{path}\")` 应被拒绝（修前是 exit 0 完全成功）; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("sandbox denied") && got.contains("path traversal"),
            "`file.chdir(\"{path}\")` 应报 sandbox denied; 实得: {got}"
        );
    }
}

/// **`chdir` 配对**：**沙箱内**的 `chdir` 仍可用，且之后 cwd 确实变了。
///
/// 这是「守卫不是一刀切禁掉 chdir」的证据 —— 只钉拒绝会让人
/// 以为应该干脆把 `chdir` 整个删掉。
#[test]
fn d334_chdir_inside_sandbox_still_works() {
    let body = "print(file.chdir(\"src\"))\nprint(file.cwd())\n";
    let (code, got) = ev(body);
    assert_eq!(code, 0, "沙箱内 chdir 应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "nil | D:\\Github\\mora-lang\\src",
        "沙箱内 chdir 应成功且 cwd 改变; 实得 {got}\n\
         （若此条红而上面那条也红，说明守卫过严；若此条红而上条绿，\
         说明 chdir 被禁了 —— 那是过度修复）"
    );
}

/// **`rename` / `copy` 专项**：**两个路径都要查**。
///
/// 少查任一个都能被用来把数据搬出沙箱，故这两条分别覆盖
/// 「源在沙箱外」与「目标在沙箱外」两种方向。
#[test]
fn d334_rename_and_copy_check_both_endpoints() {
    for (method, body) in [
        ("rename", OUT_RENAME_OUT),
        ("rename", OUT_RENAME_IN),
        ("copy", OUT_COPY_OUT),
        ("copy", OUT_COPY_IN),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 1,
            "`file.{method}` 只要**任一端**越界就该拒绝; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("sandbox denied") && got.contains("path traversal"),
            "`file.{method}` 双向都应被拦; 实得: {got}"
        );
    }
}

/// **D409 对账（e2e）**：默认策略下**跨盘路径可读** —— v0.x 就是「无沙箱」。
///
/// 这条与上面所有「拒绝」断言**刻意相反**，两者一起才说得清：
/// - 越界（`..`）⇒ **拒**（`check_path` 的 `..` 规则，明文决定）；
/// - 跨盘（`C:/…`）⇒ **放行**（`permissive()` 真的无限制，spec 17.1）。
///
/// 第一版的 D334 判据把跨盘当成「必须被拒」，那是**照着 bug 写的**。
#[test]
fn d409_default_policy_allows_cross_drive_reads() {
    let (code, got) = ev("print(file.exists(\"C:/Windows/win.ini\"))\n");
    assert_eq!(
        code, 0,
        "默认策略下跨盘 `exists` 应成功（v0.x 无沙箱，spec 17.1）; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got, "true",
        "`C:/Windows/win.ini` 应存在且可访问; 实得 {got}\n\
         若实得是 sandbox denied，说明 D409 的 `is_unrestricted` 哨兵被回退了"
    );
}

/// **对照组 1**：**沙箱内**的正常读写**不受影响**。
///
/// ⚠ 探针文件写在 `%TEMP%`，而 fs_root 是工作区盘（实测 `\\?\D:\`），
/// 故用**工作区内**的相对路径来测「沙箱内」。
#[test]
fn d334_in_sandbox_ops_still_work() {
    let body = "file.write_text(\"d334_probe.txt\", \"hello\")\nprint(file.read_text(\"d334_probe.txt\"))\nprint(file.exists(\"d334_probe.txt\"))\nprint(file.is_file(\"d334_probe.txt\"))\nprint(file.size(\"d334_probe.txt\"))\nprint(file.touch(\"d334_probe2.txt\"))\nprint(file.exists(\"d334_probe2.txt\"))\nprint(file.rename(\"d334_probe2.txt\", \"d334_probe3.txt\"))\nprint(file.copy(\"d334_probe.txt\", \"d334_probe4.txt\"))\nprint(file.remove(\"d334_probe4.txt\"))\nprint(file.remove(\"d334_probe.txt\"))\nprint(file.remove(\"d334_probe3.txt\"))\n";
    let (code, got) = ev(body);
    assert_eq!(code, 0, "沙箱内操作应全部成功; 实得 exit={code} out={got}");
    assert!(
        !got.contains("sandbox denied"),
        "沙箱内操作**不得**被守卫误伤; 实得: {got}"
    );
    assert_eq!(
        got, "hello | true | true | 5.0 | nil | true | nil | nil | nil | nil | nil",
        "沙箱内全链路现状; 实得 {got}"
    );
    // 确认探针没留下
    let (_, got2) = ev("print(file.exists(\"d334_probe.txt\"))\n");
    assert_eq!(got2, "false", "探针文件应已清理; 实得 {got2}");
}

/// **对照组 2**：**纯字符串**入口不守卫（不触碰文件系统）。
///
/// `join` / `basename` / `dirname` / `extname` / `abs` 不做任何 I/O，
/// 给它们加守卫只会误报。`abs` 尤其要注意：它**不做解析检查**，
/// `file.abs("C:/x")` 正常返回——这是「拼接」而非「访问」。
#[test]
fn d334_pure_string_ops_are_not_guarded() {
    for (body, want) in [
        ("print(file.join(\"a\", \"b\", \"c\"))\n", "a\\b\\c"),
        ("print(file.basename(\"/a/b/c.txt\"))\n", "c.txt"),
        ("print(file.dirname(\"/a/b/c.txt\"))\n", "/a/b"),
        ("print(file.extname(\"/a/b/c.txt\"))\n", ".txt"),
        ("print(file.abs(\"C:/x\"))\n", "C:/x"),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "`{body}` 应成功（纯字符串运算，不该被守卫误伤）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{body}` 应得 {want}; 实得 {got}");
    }
}

/// **源码侧的全称覆盖检查**：每个带路径的入口都必须调 `check_path`。
///
/// 行为侧判据只能覆盖**已枚举**的入口；这条按源码结构**全称**核对，
/// 防止将来新增入口时再漏。
///
/// ⚠ 解析方式的选择本身是本条的关键教训：**前一版用手写字符解析器，
/// 结果静默解析到 0 个 arm** —— 「全称判据什么都没检查却看起来通过」
/// 比红更危险。现改为**按行扫描**，并在末尾断言「至少覆盖 N 个」，
/// 让解析失效变成**显式红**。
#[test]
fn d334_every_new_op_must_call_check_path() {
    let src = include_str!("../src/interpreter/builtins/file.rs");
    // 纯字符串 / 无路径入口：允许缺守卫
    const NO_PATH: &[&str] = &[
        "cwd", "home_dir", "join", "basename", "dirname", "extname", "abs",
    ];

    let mut cur: Option<(String, Vec<String>)> = None;
    let mut arms: Vec<(String, Vec<String>)> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        // 去掉已知前缀再找右引号（clippy `manual_strip`）：
        // `t[1..]` 测过前缀后再 `find` 前缀字符，属「手动剥前缀」。
        let t = t.strip_prefix('"').unwrap_or(t);
        if let Some(q) = t.find('"') {
            let name = &t[..q];
            let rest = t[q + 1..].trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && rest.starts_with("=>")
            {
                if let Some(prev) = cur.take() {
                    arms.push(prev);
                }
                cur = Some((name.to_string(), Vec::new()));
                continue;
            }
        }
        if let Some((_, body)) = cur.as_mut() {
            body.push(line.to_string());
        }
    }
    if let Some(prev) = cur {
        arms.push(prev);
    }

    let mut checked: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut too_few: Vec<String> = Vec::new();
    // 双路径入口：`rename` / `copy` 必须**查两个**端点
    const TWO_PATH: &[(&str, usize)] = &[("rename", 2), ("copy", 2)];
    for (name, body) in &arms {
        if name == "other" || NO_PATH.contains(&name.as_str()) {
            continue;
        }
        let joined = body.join("\n");
        let takes_path = joined.contains("expect_str(")
            || joined.contains("args[")
            || joined.contains("args.get(");
        if !takes_path {
            continue;
        }
        let calls = joined.matches("check_path(").count();
        if calls == 0 {
            missing.push(name.clone());
        } else if let Some((_, want)) = TWO_PATH.iter().find(|(n, _)| n == name) {
            // ⚠ 只查「有没有」不够：D334 牙齿验证时摘掉 `rename` 的**第二处**
            // 守卫，「arm 里有 check_path」这一条**照样通过** ——
            // 「全称判据看起来在检查，实际只检查了存在性」是它自己的失效模式。
            // 故双路径入口按**次数**判定。
            if calls < *want {
                too_few.push(format!("{name}: {calls} 处（应 {want} 处）"));
                continue;
            }
        }
        checked.push(name.clone());
    }

    assert!(
        missing.is_empty() && too_few.is_empty(),
        "以下带路径入口的 sandbox 守卫**不足**——与 D334 的四个漏网入口同型。\n\
         `file.rs` 顶部承诺「enforce sandbox on every path-bearing file op」，\n\
         新增带路径的入口时 `check_path` 是**义务**而非惯例。\n\
         完全没有: {missing:?}\n次数不足: {too_few:?}\n已覆盖 {n} 个: {checked:?}",
        missing = missing,
        too_few = too_few,
        n = checked.len(),
    );
    assert!(
        checked.len() >= 16,
        "本条只检查到 {} 个带路径入口 —— **解析逻辑失效了**（file.rs 结构变了？）。\n\
         ⚠ **「全称判据静默通过 0~1 个」比「红」更危险**：\n\
         它看起来像通过，实际什么都没检查。至少应覆盖 16 个。\n\
         扫到的全部 arm: {all:?}",
        checked.len(),
        all = arms.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
    );
}

// ── 未钉的观察（诚实记录，不当判据） ──
//
// **Windows 扩展长度前缀**（两个反斜杠 + 问号）在 Mora 字符串里的行为
// **未查清**，本条不钉：
//
// ```text
// file.exists("C:/Windows/win.ini")   -> sandbox denied ... escapes fs_root   <- 正确
// file.exists("\\?\C:\Windows\win.ini") -> false（**不是** sandbox denied）
// ```
//
// 从诊断输出看，Mora 字符串字面量把两个反斜杠 + 问号处理成了**一个**
// 反斜杠 + 问号，于是该路径不再是合法的 Windows 扩展长度前缀，
// OS 层直接返回 false。
// => 现象落在**字符串转义层**，**不是** `check_path` 的缺口 ——
// `check_path` 的实现（`src/sandbox/mod.rs:109`：先拒 `..`、
// 再按 `is_absolute()` + `starts_with(canonical_root)` 判定）对普通路径是正确的，
// 而盘级绝对路径实测确被它拦住。
//
// **但**：若将来某条路径能以**完整**形式到达 `check_path`，
// 它是绝对路径且不以 fs_root 开头 => 仍会被拒。真正的风险只在
// 「字符串层能产出完整前缀 + OS 层认它」这一组合上，**本轮未测到**。
// 若要钉，应先查清 lexer 对反斜杠的转义规则，再定用例 —— 不要凭现象猜。

// ── 判据装置的三个 Windows 陷阱（写在这里提醒后来者） ──
//
// (1) **保留设备名**：`basename` / `dirname` / `exists` / `remove` slug 化后
//     落进 `CON` / `NUL` / `AUX` / `COM1`… 的形态 => `os error 123`。
// (2) **路径长度上限 260**：多行探针 body 整段 slug 化会超限 => 同样 `os error 123`。
//
// 前两者的症状都是「判据红 + 产品完全正常」，详见 `slug()` 的注释。
//
// (3) **POSIX 路径在 Windows 上无意义**：`/etc` 解析成当前盘下的 `D:\etc`
//     （**仍在沙箱内**），实测得 OS 错误而非 `sandbox denied`。
//     写沙箱判据只能用**盘级绝对路径**。

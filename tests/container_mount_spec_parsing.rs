//! v0.104.6 D389 —— `MountSpec::parse` **认不出 Windows 盘符**，
//! 导致 Windows 上任何绝对 host path 都不可用，且报错**归因到错误的字段**
//!
//! ## 缺陷
//!
//! `parse` 用 `splitn(3, ':')` 切 `host:container[:mode]`，注释写「允许 path 含 `:`」，
//! 但 **Windows 盘符本身就是冒号** —— `C:\data` 的第 2 个字符就是 `:`。
//! 于是 `"C:\data:/data:ro"` 被切成 `["C", "\data", "/data:ro"]`：
//!
//! | 字段 | 修前实测值 | 用户写的值 |
//! |---|---|---|
//! | `host_path` | `"C"` | `C:\data` |
//! | `container_path` | `"\data"` | `/data` |
//! | `mode` | `"/data:ro"` | `ro` |
//!
//! `validate()` 于是报（**真实 CLI 实测**）：
//!
//! ```text
//! sandbox.containerize: mount.mode must be 'ro' or 'rw', got: /data:ro
//! ```
//!
//! 两个问题叠在一起：
//!
//! 1. **功能全废** —— Windows 上任何**绝对** host path 都过不了校验
//! 2. **诊断误导** —— 归因到 `mode` 字段，并报出用户**从未写过**的值
//!
//! 这正是 D189（`reject_option_as_path`）的同型缺陷：**错误归因错位**。
//! 注释声称支持含冒号的路径，实现却恰好在本项目的主开发平台上失效。
//!
//! ## 修法
//!
//! **先剥掉盘符前缀再 split**，判定条件**刻意收紧**为
//! 「字母 + `:` + 路径分隔符(`\` 或 `/`)」：
//!
//! | 输入 | 是否认盘符 | 理由 |
//! |---|---|---|
//! | `C:\data:/data:ro` | 是 | `C:` 后是 `\`，是路径分隔符 |
//! | `C:/data:/data:ro` | 是 | `C:` 后是 `/` |
//! | `a:b:c` | **否** | `a:` 后是 `b`，**不是**路径分隔符 |
//! | `/data:/data:ro` | 否 | 首字符非字母 |
//!
//! 收紧的必要性：`"a:b:c"` 是**既有单测**断言的形态（1 字母 host）。
//! 若不加这个约束，`a:` 会被误当盘符 ⇒ 该单测的 `host_path` 从 `a` 变成 `a:b`，
//! **改变既有已文档化的行为**。判据 `d389_one_letter_host_not_treated_as_drive`
//! 专门守住这条。
//!
//! ## 为什么本轮不做全仓泛化
//!
//! 全仓 `splitn?(N, ':')` / `split(':')` **只有这一处**（`grep` 实证），
//! 不存在同型扩散面。

use std::path::Path;
use std::process::Command;

use mora::sandbox::{ContainerBackend, ContainerSpec, MountSpec};

fn run_fixture(name: &str) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/e2e")
        .join(name);
    let out = Command::new(exe)
        .args(["run", script.to_str().expect("路径转字符串")])
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

/// 解析 + 放进 spec 过一遍 `validate()`，返回 `(spec, validate 结果)`。
fn parse_and_validate(spec_str: &str) -> (MountSpec, Result<(), String>) {
    let m = MountSpec::parse(spec_str).expect("parse 应成功");
    let mut spec = ContainerSpec::new(ContainerBackend::Docker);
    spec.mounts.push(m.clone());
    let v = spec.validate();
    (m, v)
}

// ── ① Windows 盘符形态（本轮修复主体）──

/// **反斜杠盘符**：`C:\data:/data:ro` 三字段全部正确且 `validate` 通过。
#[test]
fn d389_windows_drive_backslash_is_parsed() {
    let (m, v) = parse_and_validate(r"C:\data:/data:ro");
    assert_eq!(m.host_path, r"C:\data", "host_path 必须含盘符");
    assert_eq!(m.container_path, "/data");
    assert_eq!(m.mode, "ro");
    assert_eq!(v, Ok(()), "Windows 绝对路径应通过校验; got={v:?}");
}

/// **正斜杠盘符**（Docker Desktop 常见写法）同样成立。
#[test]
fn d389_windows_drive_forward_slash_is_parsed() {
    let (m, v) = parse_and_validate("C:/data:/data:ro");
    assert_eq!(m.host_path, "C:/data");
    assert_eq!(m.container_path, "/data");
    assert_eq!(m.mode, "ro");
    assert_eq!(v, Ok(()), "got={v:?}");
}

/// **省略 mode 时默认 `rw`** —— 盘符剥离不得吃掉 `container` 段。
#[test]
fn d389_windows_drive_default_mode_is_rw() {
    let (m, v) = parse_and_validate(r"C:\data:/data");
    assert_eq!(m.host_path, r"C:\data");
    assert_eq!(m.container_path, "/data");
    assert_eq!(m.mode, "rw", "缺省应为 rw");
    assert_eq!(v, Ok(()), "got={v:?}");
}

/// **小写盘符**（Windows 盘符大小写不敏感，写法常见）同样被识别。
#[test]
fn d389_windows_lowercase_drive_is_parsed() {
    let (m, v) = parse_and_validate(r"c:\x:/y:rw");
    assert_eq!(m.host_path, r"c:\x");
    assert_eq!(v, Ok(()), "got={v:?}");
}

/// **盘符不得在渲染 `-v` 参数时丢失**。
///
/// ⚠ 这条**首版没有牙齿**，是牙齿验证抓出来的：`to_docker_arg()` 是用 `:`
/// **纯重新拼接**三个字段，而坏切分只是把同一段文本按冒号拆散
/// ⇒ 重新拼回去**逐字等于原输入**。所以「往返相等」这个断言在
/// **修前也成立**，完全测不出本轮缺陷。
///
/// ⇒ 断言必须落在**解析出的字段**上：只有 `host_path`/`mode` 暴露了污染。
/// （本条与 D384「探针的构造方式必须与被测性质同构」同族。）
#[test]
fn d389_drive_survives_docker_arg_rendering() {
    let m = MountSpec::parse(r"C:\data:/data:ro").expect("parse 应成功");
    assert_eq!(m.to_docker_arg(), r"C:\data:/data:ro");
    // 同一断言的**有牙齿**版本：往返相等**掩盖**了坏切分，字段才不会
    assert_eq!(m.host_path, r"C:\data", "盘符必须在字段里，不能只靠往返");
    assert_eq!(m.mode, "ro", "mode 必须是从最后一段解析出来的 ro");
}

// ── ② 反向对照：POSIX 行为**逐字未变** ──

/// POSIX / 相对路径全部与修前一致。
#[test]
fn d389_posix_forms_unchanged() {
    let (m, v) = parse_and_validate("/data:/container/data:ro");
    assert_eq!(m.host_path, "/data");
    assert_eq!(m.container_path, "/container/data");
    assert_eq!(m.mode, "ro");
    assert_eq!(v, Ok(()));

    let m2 = MountSpec::parse("/data:/data").expect("parse 应成功");
    assert_eq!(m2.mode, "rw");

    let (m3, v3) = parse_and_validate("data:/data:ro");
    assert_eq!(m3.host_path, "data");
    assert_eq!(v3, Ok(()), "got={v3:?}");
}

/// **1 字母 host 不是盘符** —— 守住既有单测 `"a:b:c"` 的语义。
///
/// 这是本轮修复**刻意收紧判定条件**的原因：若 `a:` 被误判为盘符，
/// `host_path` 会从 `a` 变成 `a:b`，**静默改变既有已文档化的行为**。
#[test]
fn d389_one_letter_host_not_treated_as_drive() {
    let m = MountSpec::parse("a:b:c").expect("parse 应成功");
    assert_eq!(
        m.host_path, "a",
        "`a:b:c` 的 host 必须是 `a` —— `a:` 后是 `b` 不是路径分隔符"
    );
    assert_eq!(m.container_path, "b");
    assert_eq!(m.mode, "c", "第三段仍应落在 mode 上（`a:b:c` 的既有语义）");
}

/// **原注释声称的「允许 path 含 `:`」在 container 段上从来不成立**。
///
/// ⚠ 首版判据把 `"/host:/da:ta:ro"` 期望成 `container_path == "/da:ta"`，
/// 实测左 `"/da"` 右 `"/da:ta"` —— **是我自己的期望写错了**，
/// 不是产品缺陷，且该行为在修前修后**完全一致**。
///
/// `splitn(3, ':')` 的真实语义：按冒号数切成**至多 3 段**
/// ⇒ 只要第 2 个冒号之后还有内容，它就**整段落进 `mode`**，
/// container 段**永远拿不到**冒号后的部分。
///
/// 也就是说三段里**没有任何一段**能真正容纳冒号（`mode` 能，但会被
/// `validate` 拒掉）。全仓**唯一**真实存在的「path 内冒号」是
/// Windows 盘符，已由 D389 显式处理。
#[test]
fn d389_container_path_cannot_hold_a_colon() {
    // 2 个冒号 ⇒ 正好 3 段，第三段吃掉余下内容
    let m = MountSpec::parse("/host:/da:ta").expect("parse 应成功");
    assert_eq!(m.host_path, "/host");
    assert_eq!(m.container_path, "/da");
    assert_eq!(m.mode, "ta", "第三个冒号之后的内容整段落进 mode");

    // 3 个冒号 ⇒ 仍是 3 段（上限 3），第三段含剩余两个冒号
    let m2 = MountSpec::parse("/host:/da:ta:ro").expect("parse 应成功");
    assert_eq!(m2.host_path, "/host");
    assert_eq!(m2.container_path, "/da");
    assert_eq!(m2.mode, "ta:ro");

    // 而这样的 mode 会被 validate 拒掉（诊断指向 mode，符合预期）
    let mut spec = ContainerSpec::new(ContainerBackend::Docker);
    spec.mounts.push(m2.clone());
    let msg = spec.validate().expect_err("mode 含冒号应被拒");
    assert!(
        msg.contains("mount.mode must be 'ro' or 'rw', got: ta:ro"),
        "got={msg}"
    );
}

// ── ③ 错误路径**仍要指向正确的字段** ──

/// 空 host 仍报 `host_path`，坏 mode 仍报 `mode` —— **没有把诊断改坏**。
#[test]
fn d389_invalid_mounts_still_name_the_right_field() {
    let (_, v_host) = parse_and_validate(":/data");
    let msg = v_host.expect_err("空 host 应被拒");
    assert!(
        msg.contains("host_path"),
        "空 host 应点名 host_path; got={msg}"
    );

    let (_, v_mode) = parse_and_validate("/data:/data:xx");
    let msg = v_mode.expect_err("坏 mode 应被拒");
    assert!(
        msg.contains("mount.mode must be 'ro' or 'rw', got: xx"),
        "坏 mode 应点名 mode 并回显用户写的值; got={msg}"
    );
}

/// 完全没有冒号 → `parse` 本身就报格式错。
#[test]
fn d389_no_separator_is_parse_error() {
    let e = MountSpec::parse("no_colon").expect_err("缺分隔符应报错");
    assert!(
        e.contains("mount spec must be 'host:container[:mode]'"),
        "got={e}"
    );
}

// ── ④ 端到端：真实 CLI 路径 ──

/// **Windows 挂载能过 `validate()`**，错误推进到 backend 门禁。
///
/// 选 `gondolin` 是为了**零副作用**：`spawn_container` 的顺序是
/// `validate()` → backend 门禁 → `docker version` 探测，
/// 所以用未实现 backend 会在**触达 docker 之前**停下
/// （本机 docker daemon 未运行，测试不能依赖它）。
#[test]
fn d389_e2e_windows_mount_reaches_backend_gate() {
    let (code, out) = run_fixture("sandbox_container_mount_win.mora");
    assert_ne!(code, 0, "未实现 backend 应非零退出; out={out}");
    assert!(
        out.contains("backend 'gondolin' not yet implemented"),
        "Windows 挂载应**通过解析与校验**并停在 backend 门禁; out={out}"
    );
    assert!(
        !out.contains("mount.mode"),
        "报错不应再指向 mount.mode —— 本轮修复的正是这个误归因; out={out}"
    );
}

/// **反向对照**：真正非法的 mode **仍然**被 `mode` 字段拦下。
///
/// 若本条也通过，说明上条只是「反正报错了」——那就失去了判别力。
#[test]
fn d389_e2e_bad_mode_is_still_caught() {
    let (code, out) = run_fixture("sandbox_container_mount_bad_mode.mora");
    assert_ne!(code, 0, "坏 mode 应非零退出; out={out}");
    assert!(
        out.contains("mount.mode must be 'ro' or 'rw', got: xx"),
        "坏 mode 应仍被 mode 字段拦下并回显用户写的值; out={out}"
    );
}

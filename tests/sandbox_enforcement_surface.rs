//! v0.104.6 D336 —— **沙箱执行面全景**（否定轮，无产品代码变更）
//!
//! D334 补齐了 `file.*` 的路径守卫覆盖，D335 修了 `check_path` 两侧形式不一致。
//! 但那两轮都只看了沙箱的**一个角**。本轮把**整张图**摊开，回答
//! 「**到底什么被强制执行了**」—— 这个问题此前的判据**从未系统回答过**。
//!
//! ## 图：四道机制，只有**一道**在执行路径上
//!
//! | 机制 | 定义位置 | 执行路径上的调用者 | 状态 |
//! |---|---|---|---|
//! | **路径守卫** `check_path` | `src/sandbox/mod.rs:109` | **仅 `file.rs`** | ✅ 真正强制（D334 补齐覆盖）|
//! | **builtin 白/黑名单** `check_builtin` | `src/sandbox/mod.rs:83` | **零** | ❌ 仅查询型（CHANGELOG 13324 已记）|
//! | **能力令牌** `permits` / `check` | `src/sandbox/capability.rs:129/269` | **零**（仅 `stress_tests.rs`）| ❌ 仅查询型 |
//! | **`fs_root` 边界** | `SandboxPolicy::permissive()` = `PathBuf::from("/")` | 硬编码 | ⚠ **无任何配置入口** |
//!
//! `grep` 实证：`.authorize(` 在整个 `src/` 树**零命中**；
//! `.permits(` 只出现在 `capability.rs` 自身（`check()` 内部 + 单测）；
//! `check_builtin` 只出现在自身定义、单测、`event/mod.rs:273` 的**注释**、
//! 以及 `typeck/dispatch.rs:817` 的**查询型 builtin** 登记。
//!
//! ## ① 能力系统内部**自洽**，但**不门控任何东西**
//!
//! ```mora
//! let t = sandbox.key("file.read")
//! sandbox.check_call(9999, "file.read")  → false   ← 未签发的 token
//! sandbox.check_call(t,    "file.read")  → true    ← 已签发
//! sandbox.check_call(t,    "web.fetch")  → false   ← 未授予的 capability
//! sandbox.token_count()                  → 1.0
//! ```
//!
//! 签发、查询、计数、撤销全部正确；**只是没有任何执行点去问它**。
//! ⇒ 这**不是**缺陷：`docs/mora-spec.md:1508-1511` 把「权限系统（类似 Deno）」
//! 明确列在 **v1.0 计划**下，且无 CLI 选项能配置 allow/deny（实测无）
//! ⇒ 用户无法「配置了一个不生效的白名单」而误以为自己受限。
//!
//! ## ② `check_builtin` 在 `permissive()` 下**对一切返回 `true`**
//!
//! ```text
//! sandbox.check_builtin("file.read_text") → true
//! sandbox.check_builtin("nonexistent.op") → true    ← 连不存在的 builtin 也 true
//! ```
//!
//! 因为 `permissive()` 的 `allow = {"*"}`。这**忠实**报告了「本策略允许一切」，
//! 但回答的是「**这个策略**允许吗」而非「**这个 builtin** 存在且被允许吗」
//! ⇒ builtin 名字在这个查询里**不起作用**。`event/mod.rs:271-274` 已记
//! 「查询结果偏宽」「目前无可观察后果」。本条把它钉成现状。
//!
//! ## ③ 路径 I/O 的**三个**入口，只有一个受守卫
//!
//! | 入口 | 文件操作 | 守卫 | 沙箱外实测 |
//! |---|---|---|---|
//! | `file.*` | `fs::read/write/create/remove` | ✅ | `sandbox denied` |
//! | **`memory.save` / `memory.load`** | `fs::write` / `fs::read_to_string` | ❌ | **完整往返成功**（D335 已报）|
//! | **`tail(path, max)`** | `fs::read_to_string` | ❌ | **读到 `C:/Windows/win.ini` 内容**（**本轮新发现**）|
//! | `import "…"` | `fs::read_to_string` | ❌ | **有意** —— 语言构造，用来加载代码 |
//!
//! `tail()` 实测：
//! ```mora
//! tail("C:/Windows/win.ini", 3)
//! → [files]
//!   [Mail]
//!   MAPI=1
//! ```
//! ⇒ **沙箱外文件内容被读出**，且 `tail` 在 `docs/mora-spec.md` 里的
//! `tail` 指的是**列表解构**（`let [head, ...tail]`）——
//! 这个**读文件**的同名 builtin **零文档**。
//!
//! **为什么本轮不补守卫**：与 D335 同一决策 —— `docs/mora-spec.md:1504`
//! 明写「当前版本**无沙箱**」。只给 `tail` 补守卫而不管 `memory.save/load`，
//! 会把不一致**放大**成三份。「路径 I/O 该守到什么范围」是**待裁决**的产品问题。
//!
//! ## ④ 顺带修正一条**给出虚假信心**的判据
//!
//! `tests/slice_count_checks.rs` 的 `d146_siblings_that_already_reject_negatives_still_do`
//! 原断言只有 `res.is_err()`，而 `run()` 把**编译错误**也映射成 `Err("COMPILE: …")`。
//! 它的 `tail` 用例写的是 `tail([1,2,3], 5, -1)` —— 首参是 list、还多一个参数，
//! 与 `tail(path, max)` 签名不符，实测报**类型不匹配**（编译期），
//! **根本没走到**负数检查。
//!
//! ⇒ 注释里「`tail(max)` 早已拒绝负数」是**未被验证**的（行为本身是对的：
//! 实测 `tail("Cargo.toml", -1)` 确实报 `max must be non-negative`）。
//! 已改为：断言**不是** `COMPILE:` + 断言错误**提到** `non-negative` + 用真实签名。

use mora::sandbox::{Capability, CapabilityStore, SandboxPolicy};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d336_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d336q_{}_{}", n, slug(body)));
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

/// **① 能力系统**：签发 / 查询 / 计数**自洽**，但**不门控执行**。
///
/// 「不门控」用**行为**钉，而不是源码：签发 token 前后，
/// 一次跨盘读文件的**结果完全相同** —— 能力令牌对 `file.*` 毫无影响。
#[test]
fn d336_capability_token_does_not_gate_anything() {
    // 系统自洽
    let (code, got) = ev(
        "let t = sandbox.key(\"file.read\")\nprint(sandbox.check_call(9999, \"file.read\"))\nprint(sandbox.check_call(t, \"file.read\"))\nprint(sandbox.check_call(t, \"web.fetch\"))\nprint(sandbox.token_count())\n",
    );
    assert_eq!(code, 0, "能力查询应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "false | true | false | 1.0",
        "签发/未签发/未授予 三种查询必须自洽; 实得 {got}"
    );

    // **不门控**：带 token 与不带 token，判定逐字相同
    //
    // ⚠ D409：原探针用**跨盘**路径，它之所以「被拒」只因为
    // `permissive()` 意外只覆盖当前盘。修掉后跨盘放行
    // ⇒ 改用 `..`（`check_path` 的专属拒绝规则，两种语义下都成立）。
    let (c1, g1) = ev("print(file.read_text(\"../d336_absent/x\"))\n");
    let (c2, g2) =
        ev("let t = sandbox.key(\"file.read\")\nprint(file.read_text(\"../d336_absent/x\"))\n");
    assert_eq!(c1, 1, "越界读应被路径守卫拒; 实得 exit={c1} out={g1}");
    assert_eq!(c2, 1, "带 token 也应被拒; 实得 exit={c2} out={g2}");
    assert_eq!(
        g1, g2,
        "签发 capability **不得**改变 `file.*` 的判定 —— 能力系统不在执行路径上。\
         若两者不同，说明有人接上了能力门（那是**有意的**语义变更，需走 CHANGELOG）"
    );
}

/// **②** `check_builtin` 在默认（`permissive()`）策略下**对一切返回 `true`**。
#[test]
fn d336_check_builtin_is_true_for_everything_under_permissive() {
    for name in ["file.read_text", "nonexistent.op", "ai.chat", ""] {
        let body = format!("print(sandbox.check_builtin(\"{name}\"))\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 0,
            "`check_builtin(\"{name}\")` 应成功; 实得 exit={code} out={got}"
        );
        assert_eq!(
            got, "true",
            "默认策略是 `permissive()`（`allow = {{\"*\"}}`）⇒ 对**任何**名字都 true。\
             builtin 名字在这个查询里不起作用。若此条红，说明默认策略被改了 —— \
             那是**有意的**策略变更"
        );
    }
}

/// **③ 路径 I/O 三入口**：只有 `file.*` 受守卫，另两个**不经过任何检查**。
///
/// 这三条一起钉，才能说清「守卫覆盖到哪、没覆盖到哪」。
///
/// ## ⚠ D409：探针从「跨盘」换成「`..`」
///
/// 原版三条都用 `C:/Windows/win.ini`，靠「`permissive()` 意外只覆盖当前盘」
/// 才让 `file.*` 报 `sandbox denied`。D409 修掉那个 bug 后跨盘放行
/// ⇒ 必然打红。这是「被拒不构成在边界外的证据」的第四次应验。
///
/// 改用 `../d336_absent/x`（父目录下**确定不存在**的路径）：
/// - `file.*` ⇒ `sandbox denied … path traversal`（**有**守卫）
/// - `memory.load` / `tail` ⇒ **OS 错误**，绝不会是 `sandbox denied`（**无**守卫）
///
/// `..` 与根边界**正交** ⇒ 在 D409 前后的两种语义下都成立，
/// 且路径不存在 ⇒ 两种情况都不留任何真实文件。
#[test]
fn d336_only_file_ops_are_path_guarded() {
    // `file.*` —— 有守卫
    let (code, got) = ev("print(file.read_text(\"../d336_absent/x\"))\n");
    assert_eq!(
        code, 1,
        "`file.*` 应被路径守卫拒; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("sandbox denied"),
        "`file.*` 应报 sandbox denied; 实得 {got}"
    );

    // `memory.load` —— **无**守卫（报的是 OS 错误，绝不是 sandbox denied）
    let (code, got) = ev("print(memory.load(\"../d336_absent/x\"))\n");
    assert_eq!(
        code, 1,
        "`memory.load` 会读文件; 实得 exit={code} out={got}"
    );
    assert!(
        !got.contains("sandbox denied"),
        "`memory.load` **无**路径守卫 —— 它碰文件系统时根本不问沙箱。实得: {got}"
    );

    // `tail(path, max)` —— **无**守卫
    let (code, got) = ev("print(tail(\"../d336_absent/x\", 3))\n");
    assert_eq!(code, 1, "`tail()` 会读文件; 实得 exit={code} out={got}");
    assert!(
        !got.contains("sandbox denied"),
        "`tail()` **无**路径守卫; 实得 {got}"
    );
}

/// **`tail()` 的负数守卫**（D336 顺带修正的那条虚假信心判据的配套）。
///
/// 行为**本身一直是对的** —— 缺的是「判据真的验到了它」。
#[test]
fn d336_tail_rejects_negative_max_for_real() {
    let (code, got) = ev("print(tail(\"Cargo.toml\", -1))\n");
    assert_eq!(
        code, 1,
        "`tail(path, -1)` 应报错; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("non-negative"),
        "错误应点明非负校验（**这才是**验证那条守卫）; 实得: {got}"
    );
}

/// **④ `fs_root` 没有任何配置入口** —— 三条独立证据。
#[test]
fn d336_fs_root_has_no_configuration_knob() {
    // 证据 1：`sandbox.*` 的 14 个入口里没有任何能改边界的
    let src = include_str!("../src/interpreter/builtins/sandbox.rs");
    for forbidden in ["set_root", "set_fs_root", "allow", "deny", "configure"] {
        assert!(
            !src.contains(&format!("\"{forbidden}\"")),
            "`sandbox.*` 不应有 `{forbidden}` 入口 —— 若将来加了，\
             本条会红，那是**有意的**功能新增（请同步更新「fs_root 硬编码」这条记录）"
        );
    }
    // 证据 2：`mode` 是只读查询
    let (code, got) = ev("print(sandbox.mode())\n");
    assert_eq!(
        code, 0,
        "`sandbox.mode()` 应成功; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "permissive", "默认策略是 permissive; 实得 {got}");

    // 证据 3：`permissive()` 的 `fs_root` 就是 `PathBuf::from("/")`
    let p = SandboxPolicy::permissive();
    assert_eq!(
        p.fs_root.as_deref(),
        Some(Path::new("/")),
        "`permissive()` 的 fs_root 应仍是 `PathBuf::from(\"/\")` —— \
         改动它会同时改掉 D335 钉的整条边界"
    );
    // 而 `default()`（严格策略）是 `None` ⇒ 拒绝一切，但运行时不用它
    assert!(
        SandboxPolicy::default().fs_root.is_none(),
        "`default()` 的 fs_root 应仍为 None"
    );
}

/// 能力枚举本身**没有变**（防止下一个人以为整套是空壳）。
#[test]
fn d336_capability_enum_is_populated() {
    let all = Capability::all();
    assert!(
        !all.is_empty(),
        "`Capability::all()` 不应为空 —— 能力系统虽不在执行路径上，\
         但 `sandbox.key` / `check_call` 仍在用这套枚举"
    );
    let store = CapabilityStore::new();
    let id = store
        .issue([Capability::FileRead].into_iter().collect(), None)
        .expect("签发应成功");
    assert!(
        store.check(id, Capability::FileRead).is_ok(),
        "签发的 capability 应能通过自己的查询"
    );
    assert!(
        store.check(id, Capability::WebFetch).is_err(),
        "未授予的 capability 应被拒"
    );
}

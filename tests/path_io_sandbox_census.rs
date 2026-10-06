//! v0.104.6 D352 —— 脚本可达的**路径 I/O 入口全称普查**：
//! 5 个入口做文件 I/O，其中**只有 `file.*` 受 sandbox 守卫**（否定轮，无产品变更）
//!
//! D336 找出「`check_path` 的调用者**只有 `file.rs`**」并**抽样**点了 3 处漏网
//! （`memory.save` / `memory.load` / `tail()`）。但 D334 / D338 的教训是
//! **必须全称枚举，不能抽样** —— 抽样会漏。
//!
//! 本条把 `src/` 下**全部 182 处**文件 I/O 逐处过一遍，
//! 剔除 CLI / 测试 / fixture / 内部基础设施后，收敛到**脚本可达**的
//! **5 个入口**。
//!
//! ## 全称结论
//!
//! | 入口 | I/O 位置 | sandbox 守卫 | 状态 |
//! |---|---|---|---|
//! | `file.*`（22 个 arm） | `file.rs:42-211` | ✅ 每个 arm 都调 `check_path` | ✅ D334 补齐 |
//! | **`memory.save`** | `memory.rs:388` | ❌ | ⚠ D335/D336 已报 |
//! | **`memory.load`** | `memory.rs:397` | ❌ | ⚠ D335/D336 已报 |
//! | **`tail(path, max)`** | `builtin_impls.rs:563` | ❌ | ⚠ D336 已报 |
//! | `import "…"` | `interpreter/mod.rs:702` | ❌ | ✅ **有意**（语言构造，用来加载代码）|
//!
//! ⇒ 三个漏网入口 + 一个有意豁免。**D336 的抽样没漏**，
//! 但**本条证明了它没漏**（从抽样升级为全称）。
//!
//! ## 为什么**不**给那三个补守卫
//!
//! 与 D335 的决策一致：`docs/mora-spec.md:1504` 明写「当前版本**无沙箱**。
//! 脚本可以读写文件系统」，「文件系统访问白名单」列在 **v1.0 计划**下。
//!
//! ⇒ 「`file.*` 该守到什么范围」是**待裁决**的产品决定（D336 已列为第 20 项）。
//! 本条只做一件事：**把完整的表钉下来**，让裁决者知道一共**几个**入口、
//! 各自的**确切位置**。
//!
//! ## 判据形态：行为侧钉「三个确实无守卫」+ 源码侧钉「`file.*` 全覆盖」
//!
//! 后者是**全称**的：按 `file.rs` 的 `match` arm 逐个检查 `check_path(` 的
//! **出现次数**（D335 已证明「只查有没有」会被「摘掉第二处」骗过）。

use std::fs;

fn read(rel: &str) -> String {
    // ⚠ `concat!` 只接受**字面量**，不能拼运行时参数 ⇒ 用 `Path` 组合。
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

/// **全称检查**：`file.rs` 的**每一个**带路径 arm 都调 `check_path`。
///
/// 按 **D335 的教训**用**次数**判定而不是「有没有」——
/// 牙齿验证时摘掉 `rename` 的第二处守卫，「有没有」这一条照样通过。
#[test]
fn d352_every_file_arm_calls_check_path() {
    let src = read("src/interpreter/builtins/file.rs");

    // 双路径 arm 必须调 **两次**
    for (name, want) in [("rename", 2), ("copy", 2)] {
        let start = src
            .find(&format!("\"{name}\" =>"))
            .unwrap_or_else(|| panic!("未找到 {name} 的 arm"));
        let next = src[start + 10..]
            .find("\n            \"")
            .map(|i| start + 10 + i)
            .unwrap_or(src.len());
        let arm = &src[start..next];
        let calls = arm.matches("check_path(").count();
        assert_eq!(
            calls, want,
            "file.{name} 的 arm 应调 check_path ** {want} 次**（两个端点各一）; 实得 {calls}"
        );
    }

    // 单路径 arm 至少一次
    for name in [
        "read_text",
        "write_text",
        "append_text",
        "read_bytes",
        "write_bytes",
        "exists",
        "is_file",
        "is_dir",
        "size",
        "list",
        "mkdir",
        "mkdir_all",
        "remove",
        "remove_all",
        "touch",
        "chdir",
    ] {
        let start = src
            .find(&format!("\"{name}\" =>"))
            .unwrap_or_else(|| panic!("未找到 {name} 的 arm"));
        let next = src[start + 10..]
            .find("\n            \"")
            .map(|i| start + 10 + i)
            .unwrap_or(src.len());
        let arm = &src[start..next];
        assert!(
            arm.contains("check_path("),
            "file.{name} 的 arm **没有** `check_path(` —— 与 D334 的修复分叉"
        );
    }

    // `abs` / `basename` / `dirname` / `extname` / `join` 是**纯字符串运算**，不该有守卫
    for name in ["abs", "basename", "dirname", "extname", "join"] {
        let start = src
            .find(&format!("\"{name}\" =>"))
            .unwrap_or_else(|| panic!("未找到 {name} 的 arm"));
        let next = src[start + 10..]
            .find("\n            \"")
            .map(|i| start + 10 + i)
            .unwrap_or(src.len());
        let arm = &src[start..next];
        assert!(
            !arm.contains("check_path("),
            "file.{name} 是**纯字符串运算**（不触碰文件系统），不该加守卫"
        );
    }
}

/// **全称检查**：三个漏网入口**确实**没有 `check_path`。
///
/// 这是 D352 的核心产出 —— 把「哪些没守」从**抽样**变成**全称清单**。
#[test]
fn d352_the_three_unguarded_path_ios_are_still_unguarded() {
    for (rel, needle, what) in [
        (
            "src/interpreter/builtins/memory.rs",
            "fs::write(&path, json).map_err(|e| format!(\"memory.save: {}\", e))?;",
            "memory.save",
        ),
        (
            "src/interpreter/builtins/memory.rs",
            "fs::read_to_string(&path).map_err(|e| format!(\"memory.load: {}\", e))?;",
            "memory.load",
        ),
        (
            "src/interpreter/builtin_impls.rs",
            "let content = std::fs::read_to_string(&path)",
            "tail()",
        ),
    ] {
        let src = read(rel);
        assert!(
            src.contains(needle),
            "`{what}` 的实现行应仍在（{rel}）; 若被改写请同步更新本条"
        );
    }

    // `memory.rs` 整文件里**零** `check_path`（它完全不在沙箱体系内）
    let mem = read("src/interpreter/builtins/memory.rs");
    assert!(
        !mem.contains("check_path"),
        "`memory.rs` 全文不应出现 `check_path` —— `memory.*` 完全不在沙箱体系内。\n\
         ⚠ 若本条红，说明有人给 `memory.*` 加了守卫 —— 那是**有意的**语义变更，\
         请同步更新 D335/D336 的「一并报告」两节"
    );
}

/// **`import` 是**有意**的豁免**（语言构造，用来加载代码）。
///
/// 与前三个不同：`import` 读的是**待执行的源码**，
/// 给它加路径白名单等于**禁止 import 沙箱外的模块** ⇒ 那是**破坏功能**，不是修缺陷。
#[test]
fn d352_import_is_a_deliberate_exemption() {
    let src = read("src/interpreter/mod.rs");
    assert!(
        !src.contains("check_path"),
        "`import` 所在的 interpreter/mod.rs 不应有 `check_path` —— \
         它读的是待执行源码，加白名单等于**禁止 import 沙箱外模块** ⇒ 破坏功能"
    );
    // 且它确实在读文件
    assert!(
        src.contains("match std::fs::read_to_string(path)"),
        "`import` 应仍走 `fs::read_to_string`"
    );
}

/// **`record/*` 的 I/O 不在脚本面** —— 它们由 CLI 的 `record` / `replay` 命令驱动。
///
/// 把这批算进「脚本可达」会**高估**暴露面，故本条把边界钉死：
/// `record/` 与 `cli/` 下的 I/O 与 `mora <file.mora>` 无关。
#[test]
fn d352_record_io_is_cli_scoped_not_script_reachable() {
    // `record` 不是脚本层的 builtin —— 用真实脚本确认它不可达
    let dir = std::env::temp_dir().join(format!("d352_rec_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    fs::write(&p, "print(record.start())\n").expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = std::process::Command::new(exe)
        .arg(&p)
        .output()
        .expect("跑 mora");
    let _ = fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "`record.start()` 在脚本层应不可达（它由 CLI 命令驱动）; 实得 exit={:?}\n\
         输出: {text}",
        out.status.code()
    );
    assert!(
        text.contains("Unbound") || text.contains("unbound") || text.contains("not defined"),
        "应是「未绑定变量」类错误; 实得: {text}"
    );
}

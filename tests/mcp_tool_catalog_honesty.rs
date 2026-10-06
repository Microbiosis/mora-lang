//! v0.104.6 D186：`mora mcp tool-list` 是一张**静态名字目录**，却以
//! 「MCP Tools (13)」的权威口气报出 —— 含 2 个**全仓无实现**的名字（已修）。
//!
//! ## 缺陷一：宣称做到了做不到的事
//!
//! 目录里列了 `ai.stream` 与 `ai.create`，但两者**全仓没有任何实现**：
//!
//! ```text
//! $ mora mcp tool-list                  →  ai.stream / ai.create 在列
//! $ let q = ai.stream(p"hi")            →  Runtime error: Unknown method: AiChat.stream
//! $ let q = ai.create("x", {})          →  Runtime error: Unknown method: AiChat.create
//! ```
//!
//! MCP 是**外部 agent 发现能力的主入口**。照着 13 个名字给客户端接线，
//! 其中 2 个必然调不通。
//!
//! `ai.create` 已由 `tests/ai_namespace_reachability.rs`（D59）记为源码不可达；
//! `ai.stream` 的 `Value::Stream` 更是一个**从未被构造**的死变体（同 D59）。
//!
//! ## 缺陷二：一个**计数**被当成系统状态
//!
//! 它读的是 `builtin_toolsets()` 这张硬编码目录，与真正对外暴露的工具
//! **毫无关系** —— 后者由每个脚本自己 `server.tool(name, schema, handler)`
//! 注册。判别性实测（同一台机器、同一个二进制）：
//!
//! ```text
//! $ mora mcp tool-list            →  MCP Tools (13)        ← 静态目录
//! $ # 一个只注册了 greet 的脚本：
//!   stderr: [mcp] Registered 1 tool(s) (1 enabled)
//!   tools/list 应答: {"tools":[{"name":"greet"}]}       ← 真实注册表
//! ```
//!
//! 同一件事，两个**都被当作权威**的计数，互不引用 —— 用户无从判断该信哪个。
//!
//! ## 修法
//!
//! 1. 删掉 `ai.stream` / `ai.create`（无实现，宣称即撒谎）；
//! 2. 标题改为「Builtin MCP tool names by toolset」，并**指明真正的事实源**
//!    （问那个服务器要 `tools/list`，或看它启动时的
//!    `[mcp] Registered N tool(s)`）；`tool-search` 同步同一口径。
//!
//! ## 判据：目录里**每个名字都必须真的能调**
//!
//! 这比「某个名字在不在列表里」强得多 —— 它把「宣称」与「实际」绑在一起，
//! 将来任何人往目录里加一个没实现的名字，这条测试立刻红。

use std::path::{Path, PathBuf};
use std::process::Command;

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

fn mora_in(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .args(args)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

fn tool_list() -> String {
    let (out, code) = mora_in(&std::env::temp_dir(), &["mcp", "tool-list"]);
    assert_eq!(code, 0, "mcp tool-list 应成功:\n{}", out);
    out
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d186_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, file: &str, body: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, body).expect("写文件");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 列出目录里「能安全实测调用」的名字 → 一个必然成功、不产生副作用的调用式。
///
/// ⚠ 这里**不**放 `ai.stream` / `ai.create` —— 它们没有实现，任何调用都会
/// 失败，正是本测试要证明的那件事。
const CALLABLE: &[(&str, &str)] = &[
    ("ai.chat", "ai.chat(p\"hi\")"),
    ("ai.critic", "ai.critic(\"answer\")"),
    ("json.parse", "json.parse(\"{}\")"),
    ("json.stringify", "json.stringify(1)"),
    ("file.read_text", "file.read_text(\"x.txt\")"),
    ("file.write_text", "file.write_text(\"x.txt\", p\"hi\")"),
    ("file.exists", "file.exists(\"x.txt\")"),
    ("file.list", "file.list(\".\")"),
    ("file.mkdir", "file.mkdir(\"sub\")"),
    ("file.remove", "file.remove(\"sub\")"),
];

/// **主判据（有牙齿）**：目录里的每个名字都必须**真的能调**。
///
/// 修前 `ai.stream` / `ai.create` 在目录里，调用会得到
/// `Unknown method: AiChat.stream` / `.create`。
#[test]
fn d186_every_catalogued_tool_name_is_actually_callable() {
    let list = tool_list();
    let dir = WorkDir::new("callable");
    // 预备前提：`file.read_text` 读的是存在的文件，不存在时会（正确地）报错
    // —— 那是**产品行为对**，不是「方法不可调」。我第一版就漏了这个准备，
    // 测试红而产品没错（D176 同款：先问「谁错了」）。
    dir.write("x.txt", "hi");

    let mut claimed_but_dead: Vec<String> = Vec::new();
    for (name, expr) in CALLABLE {
        // 先确认它在目录里（否则这条对它无意义）。
        if !list.lines().any(|l| l.trim_start().starts_with(name)) {
            claimed_but_dead.push(format!("{name} —— 不在目录里（判定应更新表）"));
            continue;
        }
        let script = dir.write("probe.mora", &format!("let q = {expr}\nprint(1)\n"));
        let (out, code) = mora_in(dir.path(), &[script.to_str().unwrap()]);
        let dead = out.contains("Unknown method")
            || out.contains("Undefined function")
            || out.contains("not implemented");
        assert!(
            !dead && code == 0,
            "目录里列了 `{}`，但调用 `{}` 失败 —— 自省不能宣称做不到的事:\n{}",
            name,
            expr,
            out
        );
    }
    assert!(
        claimed_but_dead.is_empty(),
        "以下名字的实测调用与目录不一致: {:?}",
        claimed_but_dead
    );
}

/// 曾经**无实现**的名字不得回到目录里。
///
/// ⚠ **牙齿分工要说清楚**：`d186_every_catalogued_tool_name_is_actually_callable`
/// 覆盖不了这两个名字 —— 它遍历的是 `CALLABLE` 表，而 `ai.stream` / `ai.create`
/// **被刻意排除在那张表外**（去调它们必然失败，那正是缺陷本身）。
/// 所以「宣称了做不到的事」这一半，**由本条守**；
/// 上面那条守的是「它列的东西确实能用」。两条合起来才是完整判据。
#[test]
fn d186_unimplemented_names_are_absent_from_the_catalog() {
    let list = tool_list();
    for name in ["ai.stream", "ai.create"] {
        assert!(
            !list.lines().any(|l| l.trim_start().starts_with(name)),
            "`{}` 全仓无实现（`Value::Stream` 是从未构造的死变体），\
             不得回到对外报出的工具名单里:\n{}",
            name,
            list
        );
    }
}

/// 输出**不得**以「这是某个服务器的工具清单」的口气出现，
/// 且必须指向真正的事实源。
///
/// 修前首行是 `MCP Tools (13):` —— 一个读起来像系统状态的计数。
#[test]
fn d186_tool_list_labels_itself_as_a_catalog_not_a_registry() {
    let list = tool_list();
    assert!(
        !list.contains("MCP Tools ("),
        "不得用「MCP Tools (N)」这种像状态报告的口径 —— 那是静态目录:\n{}",
        list
    );
    assert!(
        list.contains("名字目录"),
        "必须如实标注这是名字目录:\n{}",
        list
    );
    assert!(
        list.contains("tools/list") || list.contains("Registered"),
        "必须指出真正的事实源（问服务器要 tools/list / 看它的 Registered 日志）:\n{}",
        list
    );
}

/// `tool-search` 与 `tool-list` 共用同一张表，故必须共享同一口径。
#[test]
fn d186_tool_search_shares_the_same_honesty() {
    let (out, code) = mora_in(
        &std::env::temp_dir(),
        &["mcp", "tool-search", "zzz-nothing"],
    );
    assert_eq!(code, 0, "tool-search 应成功:\n{}", out);
    assert!(
        !out.contains("No tools found"),
        "空结果时须说「无匹配的名字」而非「无工具」:\n{}",
        out
    );
}

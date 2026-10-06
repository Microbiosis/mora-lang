//! v0.104.6 D403 —— `src/toolplane/` 首次外部覆盖：逻辑**零缺陷**（否定轮）
//! + 模块 doc 的 builtin 名**三处皆错**（文档更正）+ 一个**空测试**被补强
//!
//! ## ① 逻辑：否定轮
//!
//! `PlaneKind::parse` / `ToolPlane::register` / `ToolPlaneRegistry::create_plane`
//! / `find_tool` / `list_planes`（**已排序** ✓）/ `default_registry`
//! 逐条核对全部正确。
//!
//! ## ② 文档更正：doc 声称的 builtin 名**三处皆错**
//!
//! doc 原本写「调度通过 `tool.plane.dispatch(plane, name, args)`」与
//! 「builtin `tool.plane.*` 操作 plane」：
//!
//! | doc 声称 | 实际 |
//! |---|---|
//! | `tool.plane.*` | **`tool.*`**（注册名；`toolplane` 是**未绑定变量**） |
//! | `tool.plane.dispatch(…)` | **无 `dispatch` 方法** |
//! | （未列全） | 实际 8 个：`create`/`register`/`unregister`/`list`/`list_tools`/`info`/`find`/`remove` |
//!
//! D74 已因「按枚举变体名 `toolplane` 建表」踩过同一个坑，doc 又踩了一次。
//!
//! ## ③ 三兄弟的错误风格不一致（只报告，不改）
//!
//! 对**不存在的 plane**：`info` / `find` 返 `Nil`（当「没结果」），
//! `list_tools` 抛 `Err`（当「出错」）⇒ 脚本里前者静默、后者直接终止。
//! 各自与 typeck 声明一致（`info`/`find` 是 `Union[..., Nil]`，`list_tools`
//! 是裸 `List[String]`）⇒ **改它属产品语义决定**，本轮只钉现状。
//!
//! ## ④ 一个**空测试**被补强
//!
//! `runtime::sandbox::tests::tool_planes_default_has_core` 函数名声称
//! 「默认含 core plane」，函数体却只有 `let _ = &*planes;`（不 panic 即可）——
//! **什么都没断言**。旁边的注释还把 `ToolPlaneRegistry::default()` 说成
//! 「含 2 core planes」，而第 39 行用的是 `crate::toolplane::default_registry()`；
//! 派生的 `Default` 实为**空** registry ⇒ **注释与代码矛盾**。本轮补强为真断言。

use std::path::Path;
use std::process::Command;

use mora::toolplane::{PlaneKind, ToolPlane, ToolPlaneRegistry, ToolSpec, default_registry};

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {rel} 失败: {e}"))
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: "d".to_string(),
        parameters: r#"{"type":"object"}"#.to_string(),
    }
}

// ── ① Registry 逻辑矩阵 ──

/// **`PlaneKind::parse` 往返** + 非法值返回 `None`。
#[test]
fn d403_plane_kind_parse_matrix() {
    for k in [PlaneKind::Core, PlaneKind::Extension] {
        assert_eq!(PlaneKind::parse(k.as_str()), Some(k));
    }
    assert_eq!(
        PlaneKind::parse("ext"),
        Some(PlaneKind::Extension),
        "ext 是别名"
    );
    assert_eq!(
        PlaneKind::parse("EXT"),
        Some(PlaneKind::Extension),
        "应大小写不敏感"
    );
    assert_eq!(PlaneKind::parse("nope"), None);
}

/// **plane 名的校验矩阵**：空名拒、重复拒。
#[test]
fn d403_plane_name_validation() {
    let mut r = ToolPlaneRegistry::new();
    assert!(r.create_plane("p".to_string(), PlaneKind::Core).is_ok());
    let e1 = r
        .create_plane("p".to_string(), PlaneKind::Core)
        .expect_err("重名应拒");
    assert!(e1.contains("already exists"), "实得 {e1}");
    let e2 = r
        .create_plane("".to_string(), PlaneKind::Core)
        .expect_err("空名应拒");
    assert!(e2.contains("empty"), "实得 {e2}");
    assert_eq!(r.plane_count(), 1, "被拒的创建不得留下 plane");
}

/// **tool 注册**：重名拒、注销后消失。
#[test]
fn d403_tool_register_and_unregister() {
    let mut p = ToolPlane::new("t".to_string(), PlaneKind::Core);
    p.register(spec("echo")).expect("首次注册应成功");
    let e = p.register(spec("echo")).expect_err("重名 tool 应拒");
    assert!(e.contains("already exists"), "实得 {e}");
    assert_eq!(p.tool_count(), 1, "被拒的注册不得留下 tool");
    assert!(p.get("echo").is_some());
    assert!(p.unregister("echo").is_some());
    assert_eq!(p.tool_count(), 0);
    assert!(p.unregister("echo").is_none(), "二次注销应返回 None");
}

/// **`find_tool` 跨 plane 查找**：同名 tool 在不同 plane 各自独立。
#[test]
fn d403_find_tool_is_scoped_to_plane() {
    let mut r = ToolPlaneRegistry::new();
    r.create_plane("p1".to_string(), PlaneKind::Core).unwrap();
    r.create_plane("p2".to_string(), PlaneKind::Extension)
        .unwrap();
    r.get_plane_mut("p1")
        .unwrap()
        .register(spec("echo"))
        .unwrap();
    assert!(r.find_tool("p1", "echo").is_some());
    assert!(
        r.find_tool("p2", "echo").is_none(),
        "p2 没有 `echo` —— 查找必须按 plane 隔离"
    );
    assert!(
        r.find_tool("nope", "echo").is_none(),
        "不存在的 plane 应返回 None"
    );
}

/// **`list_planes` 必须排序**（D385 的 `HashMap` 随机序同族，这里是对的）。
#[test]
fn d403_list_planes_is_sorted() {
    let mut r = ToolPlaneRegistry::new();
    for n in ["zeta", "alpha", "mu"] {
        r.create_plane(n.to_string(), PlaneKind::Core).unwrap();
    }
    assert_eq!(r.list_planes(), vec!["alpha", "mu", "zeta"]);
}

/// **`default_registry()` 恰好 2 个 core plane**（`ai` + `sandbox`）。
#[test]
fn d403_default_registry_has_exactly_two_core_planes() {
    let r = default_registry();
    assert_eq!(r.plane_count(), 2);
    for (name, kind) in [("ai", PlaneKind::Core), ("sandbox", PlaneKind::Core)] {
        let p = r
            .get_plane(name)
            .unwrap_or_else(|| panic!("应含 plane {name}"));
        assert_eq!(p.kind, kind, "plane {name} 应为 Core");
    }
}

// ── ② 文档更正已落地 ──

/// **doc 不得再把 `tool.plane` / `dispatch` 当作真实接口**。
///
/// ⚠ 前两版都栽在同一处：**更正文本本身必须引用错误名字**才能解释它长什么样。
/// - 第一版 `!doc.contains("tool.plane")` ⇒ 被自己的更正段落命中；
/// - 第二版查具体句式 ⇒ 被「引用」旧句的那一行命中。
///
/// ⇒ 判别规则改成**结构性的**：凡提到旧接口的行，
/// 必须同时带引用标记「」（说明它是被**引用**的旧说法，而非被断言的接口）。
#[test]
fn d403_module_doc_no_longer_claims_tool_plane_dispatch() {
    let doc = read("src/toolplane/mod.rs");
    for token in ["tool.plane.dispatch", "builtin `tool.plane.*`"] {
        for (i, line) in doc.lines().enumerate() {
            if !line.contains(token) {
                continue;
            }
            assert!(
                line.contains('「') || line.contains("不存在") || line.contains("不是"),
                "第 {} 行提到 `{token}` 却没有标明它是**被否定的旧说法**: {line}",
                i + 1
            );
        }
    }
    // 反向对照：更正说明与真实方法名都在
    assert!(doc.contains("注册名是 `tool`"), "doc 应写明注册名是 `tool`");
    for m in ["list_tools", "unregister", "register"] {
        assert!(doc.contains(m), "doc 应列出真实方法名 {m}");
    }
}

/// **真实实现与 typeck 表一致**（8 个方法，两边同源）。
#[test]
fn d403_impl_and_typeck_agree() {
    let ai = read("src/interpreter/builtins/toolplane.rs");
    let dispatch = read("src/typeck/dispatch.rs");
    let table = dispatch
        .split("const TOOL_METHODS")
        .nth(1)
        .and_then(|s| s.split("const SKILL_METHODS").next())
        .unwrap_or("");
    for m in [
        "create",
        "register",
        "unregister",
        "list",
        "list_tools",
        "info",
        "find",
        "remove",
    ] {
        assert!(
            ai.contains(&format!("\"{m}\" =>")),
            "`call_toolplane_method` 缺 `{m}` 分支"
        );
        assert!(table.contains(&format!("\"{m}\"")), "TOOL_METHODS 缺 `{m}`");
    }
    // 反向对照：`dispatch` 确实**不存在**于两边
    assert!(!ai.contains("\"dispatch\" =>"), "实现里不应有 `dispatch`");
    assert!(
        !table.contains("\"dispatch\""),
        "typeck 表里不应有 `dispatch`"
    );
}

// ── ③ 三兄弟的错误风格：钉现状（不修） ──

/// **`info` / `find` 返 `Nil`，`list_tools` 抛 `Err`** —— 现状钉。
///
/// 若将来统一了风格（例如三者都抛错或都返 Nil），本条会红，
/// 提醒同步更新 CHANGELOG 里 D403 记录的那条产品语义分歧。
#[test]
fn d403_missing_plane_error_style_differs_among_siblings() {
    let (code, out) = run_fixture("toolplane_missing_plane.mora");
    assert_ne!(code, 0, "list_tools 那条应硬报错终止; out={out}");
    assert!(
        out.contains("info_missing=nil"),
        "`info` 对缺失 plane 应静默返 nil; out={out}"
    );
    assert!(
        out.contains("find_missing=nil"),
        "`find` 对缺失 plane 应静默返 nil; out={out}"
    );
    assert!(
        out.contains("plane 'nope' not found"),
        "`list_tools` 对缺失 plane 应报点名错误; out={out}"
    );
}

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

// ── ④ 空测试已被补强 ──

/// **`tool_planes_default_has_core` 现在真的断言了**（此前是空测试）。
///
/// 钉住「默认 registry 确有 2 个 core plane」这个**名字所承诺**的性质。
#[test]
fn d403_default_registry_assertion_is_real() {
    let sb_src = read("src/runtime/sandbox.rs");
    let body = sb_src
        .split("fn tool_planes_default_has_core()")
        .nth(1)
        .and_then(|s| s.split("\n    }").next())
        .unwrap_or("");
    // ⚠ **必须先剥注释行**再断言 —— 本条自己的注释里就写着
    //   「函数体却只有 `let _ = &*planes;`」，不剥就会被自己的注释命中
    //   （D388 / D394 / D395 / D403 **第四次**同族陷阱）。
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("let _ = &*planes;"),
        "该测试又变回空断言了（`let _ = &*planes;` 不检查任何东西）。\
         实得代码体:\n{code}"
    );
    assert!(
        code.contains("plane_count()") && code.contains("get_plane(\"ai\")"),
        "该测试应真正断言默认 registry 的 core plane; 实得:\n{code}"
    );
}

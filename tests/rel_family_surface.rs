//! v0.104.6 D349 —— `rel` 族（关系/逻辑编程）的真形态是**自由函数**，
//! 且 `both([g1, g2])` 的**列表形态被 typeck 拒**（否定轮，无产品变更）
//!
//! `rel.rs` 是 `builtins/` 里**唯一从未被量过**的模块（133 行）。
//! D34 曾记过它有一批历史存量。本轮查清三件事。
//!
//! ## ① 真形态是**自由函数**，不是 `rel.*` 方法
//!
//! 我第一轮探针全写成 `rel.succeed()` / `rel.unify(1,1)`，
//! **18 条全部 `Failed to parse`**。查 `dispatch.rs:121-126` 才发现：
//!
//! ```rust
//! "unify"   => self.call_builtin_unify(args),
//! "both"    | "conde" => self.call_builtin_both(args),
//! "either"  => self.call_builtin_either(args),
//! "project" => self.call_builtin_project(args),
//! "fail"    => self.call_builtin_fail(args),
//! "succeed" => self.call_builtin_succeed(args),
//! ```
//!
//! ⇒ 正确写法是 `succeed()` / `unify(1, 1)`，**没有 `rel.` 前缀**。
//! 而 `docs/mora-spec.md` 里 `rel.` / `unify` / `Relation` **零命中**
//! ⇒ **整个族零文档**（`README.md` 亦零）。
//!
//! ## ② `both([g1, g2])` 的列表形态：实现支持，**typeck 拒**
//!
//! `rel.rs:33-45` 的 `goals_from_args` 明确实现了「若单个实参是 List
//! 且元素全为 Goal，则展开」：
//!
//! ```rust
//! if args.len() == 1
//!     && let Value::List(items) = &args[0]
//!     && !items.is_empty()
//!     && items.iter().all(|i| matches!(i, Value::Goal(_)))
//! { return Ok(items.iter().map(...).collect()); }
//! ```
//!
//! 而源码顶部的模块文档也写着「`both(g1, g2, ...)` … **亦接受单个 Goal 列表**」。
//! 但实测：
//!
//! ```text
//! both(g1, g2)     → goal   ✅
//! both([g1, g2])   → typeck 拒：expected goal, got list<goal>   ❌
//! ```
//!
//! ⇒ **typeck 与实现分叉**：文档与实现都说支持，类型层把它挡了。
//! 与 D347/D348 的「spec 与实现分叉」方向**相反**——这次是**实现有、文档有、typeck 拒**。
//!
//! ## ③ `project` 的闭包必须**先绑定**
//!
//! `project(|x| x, 1, 5)` **解析失败**（实参位不接受内联 lambda），
//! 必须写成：
//!
//! ```mora
//! let f = fn(x) => x
//! project(f, 1, 5)
//! ```

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d349_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout **全部**实质行。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d349t_{}_{}", n, slug(body)));
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
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **主断言 ①**：这六个是**自由函数**（无 `rel.` 前缀），返回 `goal`。
///
/// 钉这个是因为我第一轮**全部**写成 `rel.*`、18 条全 `Failed to parse`，
/// 而「解析失败」很容易被误读成「产品没这个功能」。
#[test]
fn d349_rel_family_are_free_functions_returning_goal() {
    for (body, want) in [
        ("print(type_of(succeed()))", "goal"),
        ("print(type_of(fail()))", "goal"),
        ("print(type_of(unify(1, 1)))", "goal"),
        ("print(type_of(unify(1, 2)))", "goal"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(
            code, 0,
            "`{body}` 应成功（自由函数，无 `rel.` 前缀）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{body}` 应得 {want}; 实得 {got}");
    }
}

/// **主断言 ②**：`both([g1, g2])` 的**列表形态被 typeck 拒**，
/// 而 `rel.rs` 的 `goals_from_args` 明确实现了它。
///
/// ⇒ **typeck 与实现分叉**。这是本轮唯一的实质发现。
#[test]
fn d349_both_with_list_is_rejected_by_typeck_though_impl_supports_it() {
    // 变参形态：可用
    let (code, got) =
        ev("let g1 = unify(1, 1)\nlet g2 = succeed()\nprint(type_of(both(g1, g2)))\n");
    assert_eq!(code, 0, "`both(g1, g2)` 应成功; 实得 exit={code} out={got}");
    assert_eq!(got, "goal", "变参形态应返回 goal; 实得 {got}");

    // 列表形态：typeck 拒（**期望**的现状）
    let (code, got) = ev("let g1 = unify(1, 1)\nlet g2 = succeed()\nprint(both([g1, g2]))\n");
    assert_eq!(
        code, 2,
        "`both([g1, g2])` 应被 typeck 拒（现状）; 实得 exit={code} out={got}\n\
         ⚠ 若本条红，说明 typeck 已放行列表形态 —— 那是**有意的**变更（追平实现）"
    );
    assert!(
        got.contains("list<goal>") || got.contains("expected goal"),
        "错误应是「期望 goal、得到 list<goal>」; 实得: {got}"
    );
}

/// **配对**：`either` 的变参形态可用（与 `both` 同族）。
#[test]
fn d349_either_variadic_works() {
    let (code, got) =
        ev("let g1 = unify(1, 1)\nlet g2 = succeed()\nprint(type_of(either(g1, g2)))\n");
    assert_eq!(
        code, 0,
        "`either(g1, g2)` 应成功; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "goal", "应返回 goal; 实得 {got}");
}

/// **配对**：`project` 的闭包必须**先绑定**（内联 lambda 解析失败）。
#[test]
fn d349_project_requires_bound_closure() {
    // 先绑定：可用
    let (code, got) = ev("let f = fn(x) => x\nprint(type_of(project(f, 1, 5)))\n");
    assert_eq!(
        code, 0,
        "`project(已绑定的闭包, …)` 应成功; 实得 exit={code} out={got}\n\
         （内联 lambda `project(|x| x, 1, 5)` **解析失败**，见下一条）"
    );
    assert_eq!(got, "goal", "应返回 goal; 实得 {got}");

    // 内联：解析失败
    let (code, got) = ev("print(type_of(project(|x| x, 1, 5)))\n");
    assert_eq!(
        code, 2,
        "`project(|x| x, …)` 内联闭包现状是**解析失败**; 实得 exit={code} out={got}\n\
         ⚠ 若本条红，说明 parser 已支持实参位内联 lambda —— 那是**有意的**语法扩展"
    );
}

/// **源码侧**：`rel` 族是**自由函数**（在 `dispatch.rs` 的自由函数表里），
/// **不在** `builtins/` 的模块 dispatcher 里。
///
/// 这条钉住 ① 的根因，并防止下一个人误以为存在 `rel.*` 模块面。
#[test]
fn d349_rel_family_is_registered_as_free_functions_not_a_module() {
    let dispatch = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/dispatch.rs"
    ))
    .expect("读 dispatch.rs");
    for name in ["unify", "both", "either", "project", "fail", "succeed"] {
        assert!(
            dispatch.contains(&format!("\"{name}\"")),
            "`{name}` 应在 `dispatch.rs` 的自由函数分派表里"
        );
    }
    // `BuiltinKind` 里**没有** `Relation`（`Type::Relation` 是另一个东西 ——
    // 它是**类型**变体，见 `dispatch.rs:254`）
    let types = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/typeck/dispatch.rs"
    ))
    .expect("读 typeck/dispatch.rs");
    assert!(
        !types.contains("BuiltinKind::Relation"),
        "`BuiltinKind` 不应有 `Relation` 变体（那会意味着存在 `rel.*` 模块面）"
    );
}

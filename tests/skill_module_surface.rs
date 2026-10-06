//! v0.104.6 D350 —— `skill.*` 7 入口矩阵：零缺陷，但钉住三条「静默但正确」的现状
//!
//! `skill.*` 是 `builtins/` 里**最后一个**从未量过的模块（100 行 / 7 入口），
//! 且含两个**网络 / 文件**面：`set_hub` / `refresh_hub`。
//!
//! ## 矩阵结论：7 入口全部可达，**零 panic、零缺陷**
//!
//! 但量出三条**看起来像缺陷、实为设计**的现状，全部**钉住**。
//!
//! ## ① `find` / `uninstall` / `load` 的实参**不检查类型**
//!
//! 实现一律是 `args.first()?.to_string()`（`skill.rs:22` / `53` / `76`）——
//! 于是**任何**值都被接受：
//!
//! ```text
//! skill.find(1)      → nil          ← 查 "1.0"，查不到 ⇒ nil
//! skill.find(nil)    → nil
//! skill.uninstall(1) → **true**     ← 删掉了名为 "1.0" 的 skill
//! ```
//!
//! 与 D344 那 8 处 `unwrap_or(Value::Nil)` 同族，**但方向相反**：
//! 那 8 处是**可选**参数（缺省合法），这三处是**必填**参数却仍不校验。
//! **仍判为现状而非缺陷**：同模块的 `install`（`args.len() < 2` 显式检查）
//! 是**严格**的 ⇒ 又是「同模块两种策略并存」——
//! 但本条的判别（依 D344 教训）必须先确认两边**都可达**：
//! `install` 的 2 参守卫与 `find` 的 `to_string()` 都在执行路径上
//! （实测 `install("a")` 报「requires 2 args」、`find(1)` 返回 nil），
//! 所以这是**真**的策略不一致，**待裁决**。
//!
//! ## ② `set_hub` **不校验路径存在性**
//!
//! ```text
//! skill.set_hub("/tmp/nosuchhub")  → **true**（路径根本不存在）
//! skill.refresh_hub()              → read /tmp/nosuchhub: 系统找不到指定的文件
//! ```
//!
//! `set_public_registry`（`skill/mod.rs:118`）只**存路径**不读盘，
//! 真正的读发生在 `refresh_hub` ⇒ **职责分离是清晰的**，
//! 属**有意设计**（setter 不做 IO，getter/refresh 报错）。
//!
//! ## ③ `uninstall` 不存在的名字返回 `false`（布尔表意）
//!
//! 与 `bus.off`（D345）同形态：返回 `Bool(removed.is_some())`。
//! 布尔返回本就表意「删没删掉」，**不报错是合理的**。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d350_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`（`skill.*` 会碰 `~/.claude` 一类的注册表路径）。
/// 取 stdout **全部**实质行。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d350s_{}_{}", n, slug(body)));
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

/// 安装一个最小可用的 skill（`MoraSkillSpec::parse` 要求 front-matter）。
const CONTENT: &str = "---\\nname: mine\\ndescription: test skill\\n---\\n\\n# Body\\n\\nhello";

/// **契约 ①**：七入口的**基本生命周期**。
#[test]
fn d350_skill_lifecycle_works() {
    let body = format!(
        "let c = \"{CONTENT}\"\n\
         print(skill.list())\n\
         print(skill.install(\"mine\", c))\n\
         print(skill.list())\n\
         print(type_of(skill.find(\"mine\")))\n\
         print(skill.uninstall(\"mine\"))\n\
         print(skill.list())\n"
    );
    let (code, got) = ev(&body);
    assert_eq!(code, 0, "全链路应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "[] | true | [mine] | dict | true | []",
        "现状：list 空 → install → find 得 dict → uninstall → list 归空; 实得 {got}"
    );
}

/// **契约 ②**：`find` 找不到返回 **nil**（不是报错、不是空 dict）。
#[test]
fn d350_find_missing_returns_nil() {
    let (code, got) = ev("print(skill.find(\"nosuch\"))\n");
    assert_eq!(code, 0, "找不到应成功返回 nil; 实得 exit={code} out={got}");
    assert_eq!(got, "nil", "现状是 nil; 实得 {got}");
}

/// **契约 ③**：`uninstall` 不存在的名字返回 **false**（布尔表意，不报错）。
///
/// 与 D345 的 `bus.off` 同形态。
#[test]
fn d350_uninstall_missing_returns_false() {
    let (code, got) = ev("print(skill.uninstall(\"nosuch\"))\n");
    assert_eq!(code, 0, "不应报错; 实得 exit={code} out={got}");
    assert_eq!(
        got, "false",
        "现状：`uninstall` 返 `Bool(removed.is_some())`; 实得 {got}\n\
         ⚠ 若本条红，说明它改成了抛错 —— 那是**有意的**语义变更"
    );
}

/// **契约 ④**：`find` / `uninstall` / `load` 的实参**不检查类型**。
///
/// 这是本条**最反直觉**的现状：必填参数也接受任意类型。
#[test]
fn d350_typed_args_are_not_validated() {
    for (body, want) in [
        ("print(type_of(skill.find(1)))", "nil"),
        ("print(type_of(skill.find(nil)))", "nil"),
        ("print(type_of(skill.uninstall(1)))", "bool"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(
            code, 0,
            "`{body}` 应成功（实参不校验，被 `to_string()` 接受）; 实得 exit={code} out={got}\n\
             ⚠ 若本条红，说明有人给这些入口加了类型检查 —— 那是**有意的**收紧"
        );
        assert_eq!(got, want, "`{body}` 应得 {want}; 实得 {got}");
    }
}

/// **配对**：`install` 的**元数**是严格检查的（与上面的「不查类型」形成对照）。
///
/// 这证明「同模块两种策略并存」是真的：必填的**个数**查了两道
/// （typeck 编译期 + 运行期 `args.len() < 2`），**类型**却一道都没查。
///
/// ⚠ 第一版我把期望写成 exit 1（运行期报错），实测是 **exit 2** ——
///    typeck 已在**编译期**拦下，运行期那道防线**根本没被走到**。
///    两道都写进断言，免得「只测到其中一道」还以为另一道不存在。
#[test]
fn d350_install_arity_is_strictly_checked() {
    // 第一道：typeck 编译期
    let (code, got) = ev("print(skill.install(\"a\"))\n");
    assert_eq!(
        code, 2,
        "`install` 少参应被 typeck 在**编译期**拒; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("Expected 2 arguments"),
        "错误应是 typeck 的元数错误; 实得: {got}"
    );
    // 第二道：运行期 `args.len() < 2`（`skill.rs:61`）—— 源码侧钉住，
    // 因为脚本层**永远走不到**它（被 typeck 先拦）。
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtins/skill.rs"
    ))
    .expect("读 skill.rs");
    assert!(
        src.contains("skill.install: requires 2 args (name, content)"),
        "运行期的元数防线应仍在（脚本层走不到，但它是**纵深防御**的第二道）"
    );
}

/// **契约 ⑤**：`set_hub` **不校验路径存在性**，错误推迟到 `refresh_hub`。
///
/// 这是**职责分离**（setter 存路径、refresher 读盘），属有意设计。
#[test]
fn d350_set_hub_defers_error_to_refresh_hub() {
    let (code, got) = ev("print(skill.set_hub(\"C:/d350_nosuchhub\"))\n");
    assert_eq!(
        code, 0,
        "`set_hub` 对不存在的路径**仍应成功**（只存路径、不读盘）; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "true", "现状返回 true; 实得 {got}");

    // 同一个路径在 refresh_hub 才报错
    let (code, got) =
        ev("print(skill.set_hub(\"C:/d350_nosuchhub\"))\nprint(skill.refresh_hub())\n");
    assert_eq!(
        code, 1,
        "`refresh_hub` 读不存在的 hub 应报错; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("refresh_hub") && got.contains("os error"),
        "错误应点名 `refresh_hub` 与 OS 错误; 实得: {got}"
    );
}

/// **契约 ⑥**：`load` 对不存在的文件**明确报错**（真文件 I/O）。
#[test]
fn d350_load_missing_file_errors() {
    let (code, got) = ev("print(skill.load(\"nosuch_skill.md\"))\n");
    assert_eq!(
        code, 1,
        "`load` 不存在的文件应报错; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("skill.load") && got.contains("os error"),
        "错误应点名 `skill.load` 与 OS 错误; 实得: {got}"
    );
}

/// **对照组**：未知方法明确报错；`install` 的 content 必须是 SKILL.md 格式。
#[test]
fn d350_unknown_method_and_bad_content_error() {
    let (code, got) = ev("print(skill.nosuch())\n");
    assert_eq!(code, 1, "未知方法应报错; 实得 exit={code} out={got}");
    assert!(got.contains("unknown method"), "应报未知方法; 实得: {got}");

    // 非 SKILL.md 格式的 content 被 parse 拒
    let (code, got) = ev("print(skill.install(\"x\", \"just plain text\"))\n");
    assert_eq!(
        code, 1,
        "非 front-matter 的 content 应报错; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("skill.install"),
        "错误应点名 `skill.install`; 实得: {got}"
    );
}

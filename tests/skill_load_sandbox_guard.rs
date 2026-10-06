//! v0.104.6 D408 —— `skill.load` 读任意调用方路径且不走 `check_path`
//! —— **本轮自我否决：守卫不施加**，只报告根因（否定轮 + 一次真实的自我否决）
//!
//! ## 缺陷（仍然成立）
//!
//! ```rust
//! "load" => {
//!     let path_str = args.first()...;
//!     let spec = crate::skill::MoraSkillSpec::load_file(&path)  // ← 无守卫
//! ```
//!
//! `file.rs` 早有明文规则（D334/D389 那一串）：
//! **「新增带路径的入口时，`check_path` 不是「惯例」而是**义务**」**。
//! 按这条读，`skill.load` **违反**了它。
//!
//! ## 为什么**没有**修（自我否决）
//!
//! 先加了守卫，跑全量门禁 ⇒ **1 失败**：
//! `skill_load_real_skill_md_file` 报
//! `sandbox denied 'C:\...\Temp\...' escapes fs_root 'D:\'`。
//!
//! ⇒ 守卫**真的收紧了功能**：跨盘加载不了 SKILL.md。
//! 而根因**不在本入口**，在 `permissive()`：它的 `fs_root = "/"` 在 Windows 上
//! 解析成**当前工作目录所在盘**（`canonicalize("/")` → `\\?\D:\` → 剥 verbatim
//! → `D:\`），而它的 doc 写的是「允许一切 builtin, **全路径, 无限制**」
//! ⇒ **文档与行为不符**。
//!
//! 「`permissive()` 该是所有盘还是当前盘」是**未定产品语义**
//! （D397/D404 纪律：产品语义分歧不擅动）。本入口不该替它选边 ⇒ 守卫不施加。
//!
//! ## 判据形态：**钉行为，不钉「有无守卫」**（D404 的延伸）
//!
//! D408 首版断言「`load_file` 之前必须有 `check_path`」—— 那等于
//! **把一个被否决的方案钉成契约**。D409 修好 `permissive()` 后该守卫
//! 才真正可施加；本文件现在**同时**钉住两侧的终态事实：
//!
//! | 事实 | 状态 |
//! |---|---|
//! | `permissive()` = 真正「全路径、无限制」（D409 修） | 钉 |
//! | `permissive()` 仍拒 `..`（**明文决定**，只报告） | 钉 |
//! | 限制性 `fs_root` 仍逐根生效（D335/D409） | 钉 |
//! | `skill.load` 有守卫且在 `load_file` 之前（D409 施加） | 钉 |
//!
//! 牙齿验证见文件末的 D409 小节说明。
//!
//! ## 顺带：`src/skill/` 本体首次外部覆盖（**否定轮，零缺陷**）

use std::path::{Path, PathBuf};

use mora::sandbox::SandboxPolicy;
use mora::skill::{MoraSkillSpec, SkillRegistry};

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {rel} 失败: {e}"))
}

fn code_only(s: &str) -> String {
    s.lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("//") {
                return "";
            }
            l.split("//").next().unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `code_only` + **剥掉 `#[cfg(test)]` 起的整个尾部**。
///
/// ⚠ 为什么要多这一步：`#[cfg(test)] mod tests { … }` **也在 .rs 文件里**，
/// 而这些单测自己会构造 `SandboxPolicy { fs_root: Some(temp) }`。
/// 只剥注释的话，**测试里的设置点会混进「生产代码设置点」的计数**。
///
/// ⚠ 反面教训（本文件自己踩的）：第一版只做了 `code_only`，
/// 于是下面那条「生产代码里 `fs_root: Some(` 只有 1 处」拿到 **0**——
/// 因为扫描循环 `if !p.is_dir() { continue }` **跳过了全部子目录**，
/// 而 `sandbox/mod.rs` 在子目录里。写成 `sites <= 1` 时 0 也过 ⇒
/// **假绿**。⇒ 普查类判据里，**上界/下界的两边都得先证明扫描器看得见东西**。
fn production_code_only(s: &str) -> String {
    let stripped = code_only(s);
    match stripped.find("#[cfg(test)]") {
        Some(i) => stripped[..i].to_string(),
        None => stripped,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mora_d408_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

// ── ① `permissive()` 真正「全路径、无限制」（D409 修掉了 D408 发现的错） ──

/// **`permissive()` 放行**当前盘**与**其它盘**的绝对路径**。
///
/// D408 实测的错：`fs_root = "/"` 被 `canonicalize` 压成 `\\?\D:\`
/// ⇒ 「无限制」退化成「当前盘」，与 doc 和 spec 17.1 双重矛盾。
/// D409 把 `/` 定为**显式哨兵**后，本条才成立。
///
/// 同时钉住反面（D335 那一半）：**同盘**绝对路径也必须放行 ——
/// 否则退化成「一切绝对路径皆拒」。
#[test]
fn d409_permissive_allows_every_drive() {
    let pol = SandboxPolicy::permissive();

    #[cfg(windows)]
    {
        let cwd = std::env::current_dir().expect("取 cwd");
        let cwd_drive = cwd
            .components()
            .next()
            .and_then(|c| match c {
                std::path::Component::Prefix(p) => p.as_os_str().to_string_lossy().chars().next(),
                _ => None,
            })
            .unwrap_or('D');
        let other = if cwd_drive == 'D' { 'C' } else { 'D' };

        // 同盘（D335 保证）
        assert!(
            pol.check_path(&format!("{cwd_drive}:/")).is_ok(),
            "同盘盘根应放行；若被拒说明 D335 的 verbatim 剥离回归了"
        );

        // 异盘（D409 修复目标）。不要求该盘真实存在 ——
        // `check_path` 只做组件比较，路径不存在时结论一致。
        let cross = format!("{other}:/");
        assert!(
            pol.check_path(&cross).is_ok(),
            "`permissive()` 的 doc 写「全路径, 无限制」、spec 17.1 写「当前版本无沙箱」，\
             但跨盘盘根 `{cross}` 被拒 —— 说明 `is_unrestricted` 哨兵没生效，\
             D409 的修法被回退了"
        );
        // 真实存在的异盘文件也该放行（更贴近 `file.exists` 的用法）
        let real_cross = std::env::temp_dir().to_string_lossy().to_string();
        if !real_cross
            .to_uppercase()
            .starts_with(&format!("{cwd_drive}:"))
        {
            assert!(
                pol.check_path(&real_cross).is_ok(),
                "真实存在的跨盘路径 `{real_cross}` 应放行; 实得 {:?}",
                pol.check_path(&real_cross)
            );
        }
    }

    #[cfg(not(windows))]
    {
        assert!(pol.check_path("/").is_ok(), "POSIX 上 `/` 应是真正的全盘根");
        assert!(
            pol.check_path("/etc/hosts").is_ok(),
            "POSIX 上 `permissive()` 应放行任意绝对路径"
        );
    }
}

/// **`permissive()` 仍拒含 `..` 的路径** —— 这是**明文决定**，钉住现状。
///
/// 模块头写「Path safety: 拒绝含 `..` 或绝对路径越界 (out of root) 的操作」。
/// D409 修的是**根边界**，`..` 与之正交 ⇒ 未擅动。
/// 若哪天决定放开，本条会红并提醒同步改模块头。
#[test]
fn d409_permissive_still_rejects_dotdot() {
    let pol = SandboxPolicy::permissive();
    for bad in ["../x", "a/../../b", "..\\x"] {
        let e = pol
            .check_path(bad)
            .expect_err("含 `..` 的路径应被拒（明文决定）");
        assert!(
            e.contains("path traversal") || e.contains("'..'"),
            "拒绝原因应点名 `..` 路径穿越; 实得 {e}"
        );
    }
}

/// **`strict()`（`fs_root = None`）仍拒绝一切** —— D409 不得放宽它。
#[test]
fn d409_strict_still_rejects_everything() {
    let pol = SandboxPolicy::strict();
    for p in ["anywhere.txt", "/tmp/x", "C:/x", "relative/path.txt"] {
        assert!(
            pol.check_path(p).is_err(),
            "`strict()` 无 `fs_root` ⇒ 应拒绝一切; 但 `{p}` 竟放行了"
        );
    }
}

/// **相对路径按 **cwd** 解析**（D409）。
///
/// 修前把相对路径拼到 `canonical_root` 上，Windows 上那是**盘根**
/// ⇒ `check_path("Cargo.toml")` 指到 `D:\Cargo.toml`，不是工作目录下的那个。
#[test]
fn d409_permissive_resolves_relative_against_cwd() {
    let pol = SandboxPolicy::permissive();
    let got = pol.check_path("Cargo.toml").expect("相对路径应放行");
    assert!(got.is_absolute(), "应归一化成绝对路径; 实得 {got:?}");

    let cwd = std::env::current_dir().expect("取 cwd");
    assert_eq!(
        got.parent(),
        Some(cwd.as_path()),
        "相对路径应落在**进程 cwd** 下; 实得 {got:?}（cwd = {cwd:?}）"
    );
}

// ── ② 守卫机制本身有效（库级，用限制性 `fs_root`） ──

/// **限制性 `fs_root` 下，根外路径被拒、根内路径放行**。
///
/// 这条证明 `check_path` **能**拦住「带路径入口的守卫」—— 也就是说
/// D408 那个待施加的守卫**并非无效摆设**，只是根因未决。
#[test]
fn d408_check_path_actually_blocks_outside_fs_root() {
    let dir = temp_dir("root");
    let inside = dir.join("SKILL.md");
    std::fs::write(&inside, "---\nname: x\ndescription: d\n---\nbody\n").expect("写文件");
    let outside = temp_dir("outside");
    let outside_file = outside.join("SKILL.md");
    std::fs::write(&outside_file, "---\nname: y\ndescription: d\n---\nbody\n").expect("写文件");

    let policy = SandboxPolicy {
        fs_root: Some(dir.clone()),
        ..SandboxPolicy::permissive()
    };
    assert!(
        policy.check_path(&inside.to_string_lossy()).is_ok(),
        "根内路径应放行"
    );
    let denied = policy.check_path(&outside_file.to_string_lossy());
    assert!(
        denied.is_err(),
        "根外路径必须被拒 —— 守卫机制无效的话，任何带路径入口的守卫都只是形式合规"
    );
    let msg = denied.expect_err("应被拒");
    assert!(
        msg.contains("escapes fs_root"),
        "拒绝原因应说明「逃出 fs_root」; 实得 {msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

// ── ③ `skill.load` 的守卫：D409 已施加（源码级） ──

/// **`skill.load` 在 `load_file` 之前调 `check_path`**（D409 施加的守卫）。
///
/// D408 时这条**做不了**（守卫会收紧跨盘加载）⇒ 当时刻意不断言它。
/// D409 修好 `permissive()` 的哨兵语义后，守卫在默认策略下是 no-op，
/// 于是可以施加 ⇒ 它与 `file.*` 的权限面终于一致。
///
/// 默认策略下守卫**观察不到**（一切路径都放行），故只能源码级钉。
#[test]
fn d409_skill_load_calls_check_path_before_reading() {
    let code = code_only(&read("src/interpreter/builtins/skill.rs"));
    let branch_at = code.find("\"load\" =>").expect("应能找到 skill.load 分支");
    let body = &code[branch_at..];
    let guard_at = body.find("check_path");
    let read_at = body.find("load_file");
    assert!(
        guard_at.is_some(),
        "`skill.load` 里没有 `check_path` —— 违反 `file.rs` 自定的\
         「带路径入口必须守卫」义务（D409 已使其可施加）。摘掉守卫本条会红。"
    );
    assert!(
        read_at.is_some(),
        "`skill.load` 里的 `load_file` 调用不见了 —— 入口语义变了"
    );
    assert!(
        guard_at.unwrap() < read_at.unwrap(),
        "`check_path` 必须在 `load_file` **之前** —— 否则读完再判是摆设"
    );
}

/// **守卫拒绝时应点名「沙箱」**（诊断质量，别与 I/O 错误混淆）。
#[test]
fn d409_skill_load_error_names_the_sandbox() {
    let code = code_only(&read("src/interpreter/builtins/skill.rs"));
    assert!(
        code.contains("sandbox denied"),
        "守卫拒绝时应说明是**沙箱**拒绝（而非含糊的 I/O 错误）"
    );
}

/// **`fs_root` 在生产代码里仍只由 `permissive()` 设置**。
///
/// 这条让「`skill.load` 与 `file.*` 的权限面同级」这句话**可验证**：
/// 此刻没有任何生产路径配置限制性 `fs_root`，所以新加的守卫**不会**
/// 单方面收紧任何现有功能。
#[test]
fn d408_fs_root_is_only_set_by_permissive_in_production() {
    // ⚠ 必须**递归**遍历：`sandbox/mod.rs` 在子目录里。
    //    第一版用 `read_dir(src)` + 跳过目录 ⇒ 扫不到任何文件 ⇒ 恒 0（假绿）。
    fn walk(dir: &Path, out: &mut Vec<(String, usize)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let n = production_code_only(&std::fs::read_to_string(&p).unwrap_or_default())
                .matches("fs_root: Some(")
                .count();
            if n > 0 {
                out.push((p.display().to_string(), n));
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    let sites: usize = hits.iter().map(|(_, n)| n).sum();
    let files = hits.len();

    assert_eq!(
        sites, 1,
        "`fs_root: Some(` 在生产代码（已剥 `#[cfg(test)]`）里出现 {sites} 处、\
         分布于 {files} 个文件: {hits:?}（应只有 `permissive()` 1 处）—— \
         若配置能力已接通，`skill.load` 与 `file.*` 的权限面就**不再同级**，\
         D408 的自我否决依据失效，① 与本条都要重写"
    );
    assert_eq!(files, 1, "设置点应集中在单个文件里");
}

/// **默认运行时确实用 `permissive()`**（前提事实）。
#[test]
fn d408_default_runtime_uses_permissive() {
    let rt = code_only(&read("src/runtime/sandbox.rs"));
    assert!(
        rt.contains("sandbox: SandboxPolicy::permissive()"),
        "`SandboxRuntime::default` 不再用 `permissive()` —— 前提变化，① 需重写"
    );
    let pol = code_only(&read("src/sandbox/mod.rs"));
    let start = pol
        .find("pub fn permissive()")
        .expect("应能找到 permissive()");
    let body = &pol[start..(start + 900).min(pol.len())];
    assert!(
        body.contains("fs_root: Some(PathBuf::from(UNRESTRICTED_FS_ROOT))"),
        "`permissive()` 不再用 `UNRESTRICTED_FS_ROOT` 哨兵 —— ① 的前提变化，\
         「无限制」的表达方式需重写。实得:\n{body}"
    );
}

// ── ④ `skill.load` 语义不变（不回归） ──

/// **真实文件加载路径照常工作**（D408 自我否决后必须仍然如此）。
#[test]
fn d408_skill_load_still_works_under_default_policy() {
    let dir = temp_dir("default");
    let skill = dir.join("SKILL.md");
    std::fs::write(
        &skill,
        "---\nname: demo\ndescription: a demo\ntrigger: d.*\n---\n\n# Body\nhello\n",
    )
    .expect("写 SKILL.md");

    let mut reg = SkillRegistry::new();
    let spec = MoraSkillSpec::load_file(&skill).expect("load_file 应成功");
    assert_eq!(spec.name, "demo");
    assert_eq!(spec.description, "a demo");
    assert_eq!(spec.trigger.as_deref(), Some("d.*"));
    assert!(spec.body.contains("# Body"));
    reg.register(spec);
    assert_eq!(reg.count(), 1);
    assert!(reg.get("demo").is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

// ── ⑤ 顺带首次外部覆盖 `src/skill/` 本体（否定轮） ──

/// **frontmatter 解析矩阵**。
#[test]
fn d408_skill_parse_matrix() {
    let ok = MoraSkillSpec::parse(
        "---\nname: n\ndescription: d\ntrigger: t.*\nunknown_key: ignored\n---\n\nbody\n",
        None,
    )
    .expect("应解析成功");
    assert_eq!(ok.name, "n");
    assert_eq!(ok.description, "d");
    assert_eq!(ok.trigger.as_deref(), Some("t.*"));
    assert!(ok.body.contains("body"));

    // 引号
    let q = MoraSkillSpec::parse("---\nname: \"q n\"\ndescription: 'd d'\n---\nb\n", None)
        .expect("引号应被剥掉");
    assert_eq!(q.name, "q n");
    assert_eq!(q.description, "d d");

    // 值里含冒号：split_once 只切第一个 ⇒ 其余保留
    let c = MoraSkillSpec::parse("---\nname: n\ndescription: a: b: c\n---\nb\n", None)
        .expect("含冒号应可解析");
    assert_eq!(c.description, "a: b: c", "值里的冒号必须保留");

    // 错误路径
    assert!(
        MoraSkillSpec::parse("no frontmatter", None).is_err(),
        "缺 `---` 应报错"
    );
    assert!(
        MoraSkillSpec::parse("---\nname: n\ndescription: d\n", None).is_err(),
        "未闭合应报错"
    );
    assert!(
        MoraSkillSpec::parse("---\ndescription: d\n---\nb\n", None).is_err(),
        "缺 name 应报错"
    );
    assert!(
        MoraSkillSpec::parse("---\nname: n\n---\nb\n", None).is_err(),
        "缺 description 应报错"
    );
    // 不得 panic：极短输入
    for s in ["", "-", "--", "---", "----\n"] {
        let _ = MoraSkillSpec::parse(s, None); // 只要求不 panic
    }
}

/// **`SkillRegistry::list()` 按名排序**（D385 原则；`mock.names` 曾栽在这里）。
#[test]
fn d408_skill_list_is_sorted() {
    let mut reg = SkillRegistry::new();
    for n in ["zeta", "alpha", "mu"] {
        reg.register(
            MoraSkillSpec::parse(&format!("---\nname: {n}\ndescription: d\n---\nb\n"), None)
                .unwrap(),
        );
    }
    let names: Vec<String> = reg.list().into_iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["alpha", "mu", "zeta"]);
}

/// **`register` 同名覆盖是明文设计**（doc 写「overwrites if same name」）。
#[test]
fn d408_skill_register_overwrites_by_design() {
    let mut reg = SkillRegistry::new();
    reg.register(MoraSkillSpec::parse("---\nname: k\ndescription: first\n---\nb\n", None).unwrap());
    reg.register(
        MoraSkillSpec::parse("---\nname: k\ndescription: second\n---\nb\n", None).unwrap(),
    );
    assert_eq!(reg.count(), 1, "同名覆盖而非新增");
    assert_eq!(reg.get("k").unwrap().description, "second", "应保留后者");
    assert!(reg.unregister("k").is_some());
    assert_eq!(reg.count(), 0);
}

/// **`load_public_registry` 未设路径时报错**（不 panic）。
#[test]
fn d408_public_registry_requires_path() {
    let mut reg = SkillRegistry::new();
    let e = reg.load_public_registry().expect_err("未设路径应报错");
    assert!(e.contains("public_registry_path not set"), "实得 {e}");
}

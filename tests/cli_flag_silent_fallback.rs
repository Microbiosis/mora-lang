//! v0.104.6 D380 —— CLI flag 解析的静默兜底（**已修**）
//!
//! D379 在测 `record export` 时发现：`main.rs` 的 flag 解析用 `_ => {}` 兜底，
//! **任何未知 flag 都被静默忽略**。这与 D344「`builtins/**` 的静默兜底普查」
//! 是**同一族**，但发生在 **CLI 层**。
//!
//! ## 危害比 D343（`plan.create` 非法 status）更直接
//!
//! 用户要 Markdown 报告（给人看），拿到 JSONL（机器格式），
//! 而**退出码 0 让他以为成功了**。`--formt` 只差一个字母，
//! 拼错的概率远高于拼对。
//!
//! ## 修复后的行为（全部 exit 1 + 指名报错）
//!
//! | 用法 | 修前 | 修后 |
//! |---|---|---|
//! | `record export r1 --format md` | Markdown ✅ | Markdown ✅（不变）|
//! | `record export r1 --formt md`（**错拼**）| JSONL，**exit 0** ❌ | **exit 1**「未知 flag `--formt`」|
//! | `record export r1 --bogus x`（未知）| JSONL，exit 0 ❌ | **exit 1** |
//! | `record export r1 md`（位置参数）| JSONL，exit 0 ❌ | **exit 1** |
//! | `record export r1 --format BOGUS`（**值**错）| JSONL，exit 0 ❌ | **exit 1**「未知导出格式」|
//! | `record export r1 --format`（缺值）| JSONL，exit 0 ❌ | **exit 1**「flag 缺少值」|
//! | `record audit r1 --polcy x` | 静默用默认 policy ❌ | **exit 1** |
//! | `record report r1 --noet x` | 静默忽略 ❌ | **exit 1** |
//! | `record export r1`（无 format）| JSONL ✅ | JSONL ✅（不变）|
//!
//! ## 改了哪几处
//!
//! | 位置 | 上下文 | 改法 |
//! |---|---|---|
//! | `main.rs` `record export` | `--format` / `--output` | 兜底 → `unknown_flag(...)` |
//! | `main.rs` `record audit` | `--policy`（无 else 的 `if`）| 加 else 分支 |
//! | `main.rs` `record report` | `--note` / `--verify` / `--output` | 兜底 → `unknown_flag(...)` |
//! | `cli/record.rs` `run_record_export` | format 值分派 | `_ => Jsonl` → 未知值报错 |
//!
//! 三处 flag 循环共用 `main.rs::unknown_flag` 一个报错出口，`--format` 等取不到值
//! 时共用 `flag_value`（修前是 `unwrap_or(default)`，让「写了 format 却没生效」
//! 与「没写 format」完全不可区分）。
//!
//! ## `main.rs` 里剩下的那一处 `_ => {}` **不是**缺陷
//!
//! `main.rs` 的 `--version` / `--help` 预扫描 match 的主语是 `args[1]`，
//! 目的是在显示 banner 之前先截获这两个选项。落进 `_` 的就是文件名、子命令名等
//! **正常输入**，不是「不认识的 flag」。保留兜底是正确的 —— 已在原地加注释钉住，
//! 免得下一个人（或未来的普查脚本）把它当缺陷改掉。
//!
//! ## 与已有防护的关系
//!
//! D189 建的 `cli::reject_option_as_path`（`cli/mod.rs`）解决的是**另一类**问题：
//! 把 `--opt=2` 这种**看起来是路径**的参数当成文件读，导致错误归因错位。
//! 它拦的是「选项出现在文件名位置」，根本进不到子命令的 flag 循环 ——
//! 与本文件拦的「进了子命令、但 flag 名不认识」**互补而非重叠**，两者都留着。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d380_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".mora").join("recordings")).expect("建临时目录");
        std::fs::write(
            d.join(".mora").join("recordings").join("r1.jsonl"),
            "{\"kind\":\"note\",\"id\":1,\"ts_ms\":100,\"message\":\"hi\"}\n",
        )
        .expect("写录像");
        WorkDir(d)
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
        let out = Command::new(exe)
            .current_dir(&self.0)
            .args(args)
            .output()
            .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push('\n');
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), s)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn is_markdown(s: &str) -> bool {
    s.lines().any(|l| l.trim_start().starts_with("# Recording"))
}
fn is_jsonl(s: &str) -> bool {
    s.lines().any(|l| l.trim_start().starts_with('{'))
}

/// **正常用法必须正常** —— 这是本文件的前提钉。
/// 收紧错误路径最容易犯的错就是把好路径一起拒了，本条是防线。
#[test]
fn d380_correct_usage_works() {
    let wd = WorkDir::new("ok");
    let (code, out) = wd.run(&["record", "export", "r1", "--format", "md"]);
    assert_eq!(code, 0, "正常用法应成功; out={out}");
    assert!(
        is_markdown(&out),
        "`--format md` 应产出 Markdown; out={out}"
    );

    // 不给 format ⇒ 默认 jsonl
    let (code, out) = wd.run(&["record", "export", "r1"]);
    assert_eq!(code, 0, "缺省应成功; out={out}");
    assert!(is_jsonl(&out), "缺省应是 JSONL; out={out}");

    // 显式 jsonl 与 markdown 两个别名都要能用
    for f in ["jsonl", "md", "markdown"] {
        let (code, out) = wd.run(&["record", "export", "r1", "--format", f]);
        assert_eq!(code, 0, "`--format {f}` 应成功; out={out}");
    }

    // -f / -o 短选项与 `--output` 落盘
    let (code, out) = wd.run(&["record", "export", "r1", "-f", "md"]);
    assert_eq!(code, 0, "`-f md` 应成功; out={out}");
    assert!(is_markdown(&out), "`-f md` 应产出 Markdown; out={out}");

    let (code, out) = wd.run(&["record", "export", "r1", "--format", "md", "-o", "x.md"]);
    assert_eq!(code, 0, "`-o x.md` 应成功; out={out}");

    // audit / report 的正常路径同样不能被误伤
    let (code, out) = wd.run(&["record", "audit", "r1"]);
    assert_eq!(code, 0, "`record audit r1` 应成功; out={out}");
    let (code, out) = wd.run(&["record", "report", "r1", "--note", "hi"]);
    assert_eq!(code, 0, "`record report r1 --note hi` 应成功; out={out}");
    assert!(out.contains("hi"), "note 应出现在报告里; out={out}");
}

/// **错拼的 flag 现在必须被拒**（修前静默回落 JSONL、exit 0）。
#[test]
fn d380_misspelled_flag_is_rejected() {
    let wd = WorkDir::new("misspell");
    let (code, out) = wd.run(&["record", "export", "r1", "--formt", "md"]);
    assert_ne!(code, 0, "错拼 flag 现在必须非零退出; out={out}");
    assert!(
        out.contains("--formt"),
        "报错必须**指名**错拼的那个 flag; out={out}"
    );
    assert!(
        !is_markdown(&out),
        "错拼的 `--formt` 绝不能**碰巧**生效; out={out}"
    );
}

/// **未知 flag / 位置参数 / 缺值 / 未知格式值，全部拒绝。**
#[test]
fn d380_unknown_flag_is_rejected() {
    let wd = WorkDir::new("unknown");
    for args in [
        vec!["record", "export", "r1", "--bogus", "x"], // 未知 flag
        vec!["record", "export", "r1", "md"],           // 位置参数（本命令不接）
        vec!["record", "export", "r1", "--format"],     // 缺值
        vec!["record", "export", "r1", "--format", "BOGUS"], // flag 对、**值**错
        vec!["record", "audit", "r1", "--polcy", "x"],  // audit 的错拼
        vec!["record", "report", "r1", "--noet", "x"],  // report 的错拼
    ] {
        let (code, out) = wd.run(&args);
        assert_ne!(
            code,
            0,
            "`{}` 现在必须非零退出（修前全部 exit 0 静默回落）; out={out}",
            args.join(" ")
        );
        assert!(
            !is_markdown(&out),
            "`{}` 应报错而非产出 Markdown; out={out}",
            args.join(" ")
        );
    }
}

/// **源码层断言**：三处 flag 循环里的静默兜底必须**已消失**，
/// 而 `--version`/`--help` 预扫描那一处必须**仍在**（它是对的）。
///
/// **`snapshot` 的 flag 解析曾是普查看不到的另一种形状**（D380 补漏）。
///
/// 那一轮我按「`_ => {}` 计数」做普查，而 snapshot 用的是
/// `args.iter().any(|a| a == "--update")` —— 形状完全不同，数不到它。
/// 实测：`mora snapshot f.mora r1 --updat`（错拼）静默忽略、exit 0。
#[test]
fn d380_snapshot_unknown_flag_is_rejected() {
    let dir = std::env::temp_dir().join("mora_d380_snap");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let script = dir.join("s.mora");
    std::fs::write(&script, "print(1)\n").expect("写脚本");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let run = |extra: &[&str]| -> (i32, String) {
        let out = Command::new(exe)
            .current_dir(&dir)
            .arg("snapshot")
            .arg(&script)
            .arg("r1")
            .args(extra)
            .output()
            .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), s)
    };

    let (code, out) = run(&["--updat"]);
    assert_ne!(
        code, 0,
        "错拼的 `--updat` 现在必须非零退出（修前静默忽略、exit 0）; out={out}"
    );
    assert!(
        out.contains("--updat"),
        "报错必须**指名**错拼的那个 flag; out={out}"
    );

    // 反向对照：正确的 `--update` 与不带 flag 都必须仍然工作
    let (code, out) = run(&["--update"]);
    assert_eq!(code, 0, "`--update` 应正常退出; out={out}");
    let (code, out) = run(&[]);
    assert_eq!(code, 0, "不带 flag 应正常退出; out={out}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 这一条是 D380 最容易被「普查脚本按行数批量改」误伤的地方 ——
/// 把三处一起删掉会让预扫描也开始报错，`mora file.mora` 直接跑不了。
#[test]
fn d380_only_the_help_prescan_keeps_its_wildcard_arm() {
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("读 main.rs");
    let count = main
        .lines()
        .filter(|l| l.trim() == "_ => {}")
        // 排除注释行里提到 `_ => {}` 的那几行
        .count();
    assert_eq!(
        count, 1,
        "main.rs 应只剩 1 处 `_ => {{}}`（`--version`/`--help` 预扫描，\
         落进 `_` 的是文件名/子命令名 —— 正常输入，不是未知 flag）; 实得 {count}"
    );

    // 三处 flag 循环**逐一**必须走 unknown_flag 出口。
    //
    // 判据演进记录：这里一开始写成 `main.matches("unknown_flag(").count() >= 4`，
    // 牙齿验证时发现**删掉 `record report` 的调用它也不红**；改成
    // `main.contains("\"record report\"")` 同样不红 —— 因为 `flag_value` 的调用里
    // 也带着同样的子命令名。所以只能用**结构**判：按大括号配对取出每个
    // `while i < args.len()` 的循环体，逐个查里面有没有 `unknown_flag(`。
    {
        let mut loops: Vec<&str> = Vec::new();
        let mut rest = main.as_str();
        while let Some(at) = rest.find("while i < args.len() {") {
            let body_start = at;
            let after = &rest[body_start..];
            let mut depth = 0i32;
            let mut end = after.len();
            for (off, ch) in after.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = off + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            loops.push(&after[..end]);
            rest = &after[end..];
        }
        assert_eq!(
            loops.len(),
            3,
            "main.rs 应恰有 3 处 `while i < args.len()` flag 循环\
             （record export / record audit / record report）; 实得 {}",
            loops.len()
        );
        for (n, body) in loops.iter().enumerate() {
            assert!(
                body.contains("unknown_flag("),
                "第 {n} 处 flag 循环里必须有 `unknown_flag` 调用；\
                 没有它的循环会静默吞掉未知 flag。循环体：\n{body}"
            );
        }
    }

    // 缺值必须报错，而不是 unwrap_or(默认值)
    assert!(
        main.contains("fn flag_value("),
        "`flag_value` 应存在：取不到值时报错，让「写了 format 却没生效」\
         不再与「没写 format」不可区分"
    );

    // D189 的 `reject_option_as_path` 仍在（两者互补，都需要）
    let mod_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/mod.rs"))
        .expect("读 cli/mod.rs");
    assert!(
        mod_rs.contains("fn reject_option_as_path"),
        "D189 建的 `reject_option_as_path` 应仍在（它解决的是**另一类**问题：\
         把 `--opt=2` 当路径读，与本文件互补）"
    );
}

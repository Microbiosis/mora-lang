//! v0.104.6 D189：选项落到**文件位置**时，报错说「文件不存在」—— 而文件就在当前目录（已修）。
//!
//! ## 缺陷：报错**归因错误**
//!
//! CLI 只扫**第一个非选项参数之前**的选项（`--opt=N` 仅在 `args[1]` 被识别，
//! 见 `main.rs`）。于是放在后面的选项会落进「文件槽」，被当路径去读：
//!
//! ```text
//! $ mora --check --opt=1 ok.mora
//! --opt=1: 系统找不到指定的文件。 (os error 2)      ← ok.mora 就在当前目录！
//!
//! $ mora --check --bogus ok.mora
//! --bogus: 系统找不到指定的文件。 (os error 2)      ← 同上
//!
//! $ mora record --update ok.mora n
//! record: failed to read --update                 ← 同上
//! ```
//!
//! 三条路径都**指向一个不存在的路径问题**。用户会去检查文件、目录、权限 ——
//! 而真正的原因是**参数写在了错误的位置**。**报错在一个可验证的事实上撒谎。**
//!
//! 与 D187/D188 实测到的探针坑同源（`--opt` 放文件后被静默忽略），
//! 但那是我**测量**时踩的；这里是**用户**会踩的路径。
//!
//! ## 修法
//!
//! 读文件前先看这个参数**是不是选项**（以 `-` 开头），是就分两种情况说清楚：
//!
//! - **已知选项** → 「选项必须写在文件**之前**」，并给正确写法的例子；
//! - **未知选项** → 「这是不认识的选项」，指向 `--help`。
//!
//! 两种都不再说「文件不存在」。
//!
//! ## 判据
//!
//! ① 每条路径都必须**拒绝**「说文件不存在」这句话（错因归错是本质）；
//! ② 已知选项与未知选项**必须被区分**（都归到同一类也只是换个错）；
//! ③ **正常用法不受影响**。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d189_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, body).expect("写脚本");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
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

/// 这句话在「文件确实存在」时出现，就是**归因错误** —— 本文件的核心判据。
fn blames_a_missing_file(out: &str) -> bool {
    out.contains("系统找不到指定的文件")
        || out.contains("No such file")
        || out.contains("failed to read --")
}

/// 落在**文件位置**的已知选项 —— 各条命令路径。
const KNOWN_OPTION_IN_FILE_SLOT: &[&[&str]] = &[
    &["--check", "--opt=1"],
    &["record", "--update"],
    &["replay", "--opt=1"],
    &["snapshot", "--update"],
    &["run", "--opt=1"],
];

/// **主判据（有牙齿）**：每条路径都**不得**把「选项写错位置」报成
/// 「文件不存在」。
///
/// 修前这五条全部报「系统找不到指定的文件」。
#[test]
fn d189_option_in_file_slot_is_not_reported_as_a_missing_file() {
    let dir = WorkDir::new("known");
    let f = dir.script("ok.mora", "print(1)\n");
    let p = f.to_str().unwrap().to_string();

    for args in KNOWN_OPTION_IN_FILE_SLOT {
        // 补齐各命令的位置参数（`record/replay/snapshot` 要 2 个）。
        let mut argv: Vec<&str> = args.to_vec();
        argv.push(p.as_str());
        if argv[0] != "run" && matches!(argv[0], "record" | "replay" | "snapshot") {
            argv.push("n");
        }
        let (out, code) = mora(dir.path(), &argv);
        assert_ne!(code, 0, "[{}] 应判失败", argv.join(" "));
        assert!(
            !blames_a_missing_file(&out),
            "[{}] 报「文件不存在」是**归因错误** —— 文件就在当前目录:\n{}",
            argv.join(" "),
            out
        );
        assert!(
            out.contains("不能出现在文件位置"),
            "[{}] 应说明「选项写在了文件位置」:\n{}",
            argv.join(" "),
            out
        );
    }
}

/// 未知选项**也不得**被说成「文件不存在」—— 那是换了个错。
///
/// 刻意**不**区分「已知选项写错位置」与「未知选项」：那需要一张按命令的
/// 选项表，而那张表必然与实现漂移（D175 / D186 同款「第二份清单」教训）；
/// 且「是否合法」本就依赖位置 —— `--update` 对 `snapshot` 合法、只是位置错，
/// 对 `record` 则根本不存在，一张按命令的表也表达不了。
#[test]
fn d189_unknown_option_is_also_not_reported_as_a_missing_file() {
    let dir = WorkDir::new("unknown");
    let f = dir.script("ok.mora", "print(1)\n");
    let p = f.to_str().unwrap();

    let (out, code) = mora(dir.path(), &["--check", "--bogus", p]);
    assert_ne!(code, 0, "应判失败:\n{}", out);
    assert!(
        !blames_a_missing_file(&out),
        "未知选项也不该报「文件不存在」:\n{}",
        out
    );
    assert!(
        out.contains("不能出现在文件位置"),
        "应说明问题在位置而非文件:\n{}",
        out
    );
}

/// **正常用法不受影响**：合法调用仍照常工作。
#[test]
fn d189_normal_invocations_are_unaffected() {
    let dir = WorkDir::new("normal");
    let f = dir.script("ok.mora", "print(1)\n");
    let p = f.to_str().unwrap();

    // `--opt` 放在文件**之前**是支持的写法，必须照常跑。
    let (out, code) = mora(dir.path(), &["--opt=1", p]);
    assert_eq!(code, 0, "正确写法的 --opt 不该被拒:\n{}", out);

    let (out, code) = mora(dir.path(), &["--check", p]);
    assert_eq!(code, 0, "普通 --check 不该被拒:\n{}", out);
    assert!(
        out.contains("No type errors found"),
        "应正常完成类型检查:\n{}",
        out
    );

    let (out, code) = mora(dir.path(), &[p]);
    assert_eq!(code, 0, "普通运行不该被拒:\n{}", out);
    assert!(out.contains("1.0"), "应正常输出:\n{}", out);
}

/// 文件**真的**不存在时，仍要照实说「找不到文件」—— 不能把守卫变成
/// 「什么都报选项错」。
#[test]
fn d189_a_genuinely_missing_file_is_still_reported_as_missing() {
    let dir = WorkDir::new("missing");
    let (out, code) = mora(dir.path(), &["--check", "definitely-not-here.mora"]);
    assert_ne!(code, 0, "应判失败:\n{}", out);
    assert!(
        blames_a_missing_file(&out),
        "文件真的不存在时仍要照实说 —— 守卫不能变成「什么都报选项错」:\n{}",
        out
    );
    assert!(
        !out.contains("不能出现在文件位置"),
        "这与选项无关，不该提选项位置:\n{}",
        out
    );
}

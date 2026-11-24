//! v0.110.0: CLI 共享编译/路径辅助与 REPL 入口。
//! main.rs 保留 dispatch + 执行入口。

pub mod bench;
pub mod bundle;
pub mod fmt;
pub mod install;
pub mod profile;
pub mod repl;
pub mod test_runner;
pub mod vendor;

use std::fs;
use std::path::Path;
use std::process;

use crate::error::{MoraError, MoraResult};
use crate::parser_v3::ParserV3;

/// 单遍编译与优化入口：`ParserV3::compile` 产出 `ControlProgram` 与 `MirWitness[]`，
/// 并按指定（或环境变量回落）优化等级执行 `mir::control::optimize_program`。
/// 调用方负责完成 `check_program_witnesses` 类型检查。
pub fn compile_and_opt(
    source: &str,
    opt_level: Option<crate::mir::control::OptLevel>,
) -> MoraResult<(
    crate::mir::control::ControlProgram,
    Vec<crate::mir::witness::MirWitness>,
)> {
    let (mut prog, witnesses) = ParserV3::compile(source)?;
    let level = opt_level.unwrap_or_default();
    crate::mir::control::optimize_program(&mut prog, level);
    Ok((prog, witnesses))
}
/// v0.104.6 D191：读源码时**剥掉开头的 UTF-8 BOM**。
///
/// BOM（U+FEFF）是**编码产物**，不是源码内容 —— 但 `ParserV3` 会如实拒绝它：
/// `Unexpected character '\u{feff}' at line 1, column 1`。于是：
///
/// - 文件：整个程序解析失败；
/// - REPL / 管道：**第一行被静默丢弃**（实测：本会话的 PowerShell 探针
///   往 stdin 注入 BOM，`print("hi")` 直接消失、无任何提示，
///   我据此在 D190 把它误记成「REPL 首行必然 parse error」的**产品缺陷**）。
///
/// 触发面很常见：Windows PowerShell 的 `Set-Content`/`Out-File`（默认 UTF-8
/// 带 BOM）、部分编辑器/工具链的「另存为」。**在工具边界容忍它**是对的：
/// 用户不会因为工具写文件时多带了一个编码标记而该收到语法错误。
///
/// 只剥**开头**一个 —— 源码中间的 U+FEFF 是真实字符，仍应被拒。
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

/// 读源码文件，剥掉 BOM；I/O 失败给出**可操作**的说明。
///
/// v0.104.6 D192：`fs::read_to_string` 的 io::Error 原样透出时是
/// `stream did not contain valid UTF-8` —— **准确但没告诉用户该做什么**。
/// 对本语言的读者（中文 Windows 生态，GBK/GB18030 仍是不少工具的默认
/// 另存为编码）这不是边角情况：
///
/// ```text
/// $ mora gbk.mora            # 用 GBK 编码写的文件
/// gbk.mora: stream did not contain valid UTF-8
/// ```
///
/// 说明「请另存为 UTF-8」是唯一有用的下一步。本函数**不**尝试猜测/转换编码
/// —— 那属语言/工具链的**设计决定**（要支持 GBK 得引入编码探测），未擅自做。
pub fn read_source(path: &Path) -> MoraResult<String> {
    fs::read_to_string(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                MoraError::Io(format!(
                    "不是有效的 UTF-8 文本（{}）。请用编辑器**另存为 UTF-8** 后重试。\n\
                 \x20 提示：Windows 的部分工具默认写 GBK/GB18030；\
                 若是刚由这类工具生成，转换编码即可。",
                    e
                ))
            } else {
                MoraError::Io(e.to_string())
            }
        })
        .map(|s| strip_bom(&s).to_string())
}

/// v0.104.6 D189：文件位置上的参数**看起来是选项**时，别再去读它。
///
/// 修前的行为是把该参数当路径去读，然后报「系统找不到指定的文件」——
/// 而**那个文件明明就在当前目录**。报错把原因**归错了**，用户会去反复
/// 检查一个根本不存在的路径问题：
///
/// ```text
/// $ mora --check --opt=1 ok.mora
/// --opt=1: 系统找不到指定的文件。 (os error 2)      ← ok.mora 就在这儿
/// $ mora record --update ok.mora n
/// record: failed to read --update                 ← 同上
/// ```
///
/// 修法只说**一件确实成立的事**，不做「已知 / 未知」的分类：
/// 分类需要一张**按命令**的选项表，而那张表必然与实现漂移
/// （D175 的 `module_method_names`、D186 的 MCP 名字目录都是同款教训）。
/// 更麻烦的是「是否合法」本就**依赖位置** —— `--update` 对 `snapshot`
/// 是合法选项、只是写错了地方，对 `record` 则根本不存在，
/// 一张按命令的表也表达不了这件事。故一律说位置问题。
pub fn reject_option_as_path(path: &str) {
    if !path.starts_with('-') || path == "-" {
        return;
    }
    eprintln!(
        "`{}` 是选项，不能出现在文件位置 —— 而报「文件不存在」会把你引向错误的排查方向。\n\
         选项要写在**文件之前**（`mora --opt=1 file.mora`）。`mora --help` 列出全部可用选项。",
        path
    );
    process::exit(2);
}

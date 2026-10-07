//! v0.01: CLI 二进制入口 — dispatch + run_file/run_check/run_repl + install/banner。
//! v0.75.53: record/mcp 子命令已迁 lib 侧 cli/，本文件仅保留执行入口与分派。

// mod ast; mod interpreter; ... 现在由 src/lib.rs 暴露

use std::env;
use std::fs;
use std::path::Path;
use std::process;

// v0.75.53: CLI 子命令迁至 lib 侧 cli/（record + mcp），main 只做 dispatch。
use mora::cli::{mcp, record};
use mora::interpreter::Interpreter;
use mora::parser_v3::ParserV3;
use mora::typeck::format_error;

/// v0.75.53: 单遍编译 + 优化已随 record/mcp 迁至 `mora::cli::compile_and_opt`。
/// 本文件仅保留编译入口 run_file/run_check/run_repl 与 CLI dispatch。
fn main() {
    // v0.104.6 D25：把整个命令分派放到**显式大栈线程**上跑。
    //
    // Windows 主线程栈由 PE 头决定，默认仅 **1 MB**（Linux 8 MB）。而
    // `ParserV3` 是标准递归下降实现 —— 一层表达式嵌套要穿过
    // `emit_or_w → and → equality → pipe → comparison → term → factor →
    // unary → call → primary` 十个栈帧，每帧还带着 `MirWitness` 与若干
    // 局部量。实测在主线程默认栈上，**20~32 层**普通嵌套就爆栈：
    //
    // ```text
    // print((((((1))))))          # 21 层 → thread 'main' has overflowed its stack
    // let s = "abc"  print(s[0][0]… )  # 29 层 → 同上
    // ```
    //
    // 而且 `mora --check` **同样崩**（它调的是同一个 `ParserV3::compile`）——
    // 即「检查通过、运行崩溃」并不成立，20 多层嵌套这种完全正常的代码
    // 直接让编译器硬崩（abort，非可读错误），连退出码都没有。
    //
    // 这里只做「把可用栈从 1 MB 抬到 64 MB」这一件事：把 20 层的崩溃
    // 变成几百层，把绝大多数真实代码彻底移出这个雷区。**真正的兜底是
    // parser 侧���嵌套深度上限**（`parser_v3::MAX_NESTING_DEPTH`）——
    // 超出时给可读诊断而非爆栈。两者配合：栈决定实际上限，深度上限
    // 保证越界时报错而不是崩。
    const STACK_SIZE: usize = 64 * 1024 * 1024;
    let code = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .name("mora-main".to_string())
            .spawn_scoped(s, dispatch)
            .map(|h| h.join().unwrap_or(101))
            .unwrap_or(101)
    });
    if code != 0 {
        process::exit(code);
    }
}

/// D380：未知 flag 静默忽略的**唯一**报错出口。
///
/// 此前 `record export` / `record report` 是 `_ => {}` 兜底、`record audit` 是
/// 无 else 的 `if`，三者都会把错拼的 flag（如 `--formt md`）**默默丢弃**、
/// 让命令回落到默认值 —— 用户看到的是「exit 0 成功」，而他根本没要求那个格式。
/// 更糟的是 `--formt` 只差一个字母，拼错的概率远高于拼对。
///
/// 与 D189 的 `reject_option_as_path` **互补而非重叠**：后者拦的是
/// 「选项出现在文件名位置」，根本进不到子命令的 flag 循环；
/// 这里拦的是「进了子命令、但 flag 名不认识」。两道都需要。
fn unknown_flag(name: &str, subcommand: &str, accepted: &[&str]) -> ! {
    eprintln!(
        "mora {subcommand}: 未知 flag `{name}`。\n\
         本命令接受的 flag：{}\n\
         （错拼的 flag 此前会被静默忽略并回落到默认值。）\n\
         `mora --help` 列出全部可用选项。",
        accepted.join(" / ")
    );
    process::exit(1);
}

/// 取 flag 的值；缺值时**报错**而不是静默保留默认值。
/// 此前 `record export r1 --format`（缺值）会 `unwrap_or(default)`，
/// 于是「写了 format 却没生效」与「没写 format」完全不可区分。
fn flag_value(args: &[String], i: &mut usize, flag: &str, subcommand: &str) -> String {
    *i += 1;
    match args.get(*i) {
        Some(v) => v.clone(),
        None => {
            eprintln!("mora {subcommand}: flag `{flag}` 缺少值。");
            process::exit(1);
        }
    }
}

fn dispatch() -> i32 {
    let args: Vec<String> = env::args().collect();

    // --version / --help 不显示 banner
    if args.len() >= 2 {
        match args[1].as_str() {
            "--version" | "-v" => {
                eprintln!("Mora v{}", mora::VERSION);
                return 0;
            }
            "--help" | "-h" => {
                println!(
                    "Mora v{} — record / replay / diff / list / stats / timeline / snapshot",
                    mora::VERSION,
                );
                println!();
                println!("Usage:");
                println!("  mora <file.mora>           Run a script");
                println!(
                    "  mora --opt=1 file.mora     Run with SSA optimization (0=off/1=basic/>=2=aggressive)"
                );
                println!("  mora --repl                Interactive REPL");
                println!("  mora --check <file>        Type check only");
                // v0.104.6 D185：补上三个**能跑却在用法里没有**的顶层子命令。
                // 此前它们只在标题行里被顺带提到（`snapshot`），`run` / `install`
                // 连标题行都没有 —— 而分派是真的（`main.rs` 的 match 臂）。
                println!("  mora run <file.mora>       Run a script (explicit form)");
                println!("  mora install <url>         Install a package from a URL");
                println!();
                println!("Recording:");
                println!(
                    "  mora record <file> <name>  Record ai.chat/web.fetch to .mora/recordings/<name>.jsonl"
                );
                println!("  mora replay <file> <name>  Replay recording (deterministic)");
                println!("  mora diff <a> <b>          Diff two recordings");
                println!("  mora record list           List all recordings");
                println!("  mora record stats <name>   Show recording statistics");
                println!("  mora record timeline <name> Show call timeline");
                // v0.104.6 D185：补上三个缺失的 `record` 子命令。其中
                // **`audit` 是密钥扫描器** —— 用户问「我这份录像里有没有泄漏
                // API key」时唯一能回答问题的命令，此前在 `--help` 的
                // 标题行与用法列表里**都没有**出现过。
                println!(
                    "  mora record export <name>  Export recording (--format jsonl|md, --output <file>)"
                );
                println!(
                    "  mora record audit <name>   Scan recording for secrets (--policy <file>)"
                );
                println!(
                    "  mora record report <name>  Generate an evidence report (--note, --verify, --output)"
                );
                println!();
                println!("Regression:");
                // v0.104.6 D185：`snapshot` 此前只在标题行出现，没有用法条目。
                println!(
                    "  mora snapshot <file> <name> [--update]   Record-and-compare regression test"
                );
                println!();
                println!("MCP:");
                println!("  mora mcp tool-list         List available MCP tools");
                println!("  mora mcp tool-search <q>   Search MCP tools");
                println!("  mora mcp toolsets          List available toolsets");
                println!();
                println!("  mora --version             Show version");
                println!("  mora --help                Show this help");
                return 0;
            }
            // D380：此处**不是** flag 循环，保留 `_ => {}` 是正确的。
            // 上面 match 的主语是 `args[1]`（只看**第一个**参数），
            // 目的是在显示 banner 之前先截获 `--version` / `--help`。
            // 落进 `_` 的就是文件名、子命令名等正常输入，不是「不认识的 flag」。
            // D380 的真实缺陷在 `record export` / `record report` /
            // `record audit` 三处子命令的 flag 循环，已改为 `unknown_flag(...)` 硬报错。
            _ => {}
        }
    }

    // 启动横幅
    print_banner();

    if args.len() < 2 {
        run_repl();
        return 0;
    }

    // v0.75.30: 显式编译选项 `--opt=N`（0=关/1=Basic/>=2=Aggressive）—
    // 从环境变量提升为 CLI 一等参数，供 run_file/run_record/run_replay/
    // run_snapshot 四个编译入口使用。未指定 → None → 各入口走 env 兜底。
    // 剥掉 flag 后重组 args，后续 match 逻辑不变（`--opt` 只在 args[1]，
    // 不进入子命令参数）。
    let opt_level: Option<mora::mir::ssa::OptLevel> = args
        .get(1)
        .and_then(|a| a.strip_prefix("--opt="))
        .and_then(mora::mir::ssa::OptLevel::from_arg);
    let mut args = args;
    if opt_level.is_some() {
        args.remove(1);
    }

    match args[1].as_str() {
        "--repl" => run_repl(),
        "--check" => {
            if args.len() < 3 {
                eprintln!("Usage: mora --check <file.mora>");
                process::exit(1);
            }
            run_check(&args[2]);
        }
        "install" => {
            if args.len() < 3 {
                eprintln!("Usage: mora install <url>");
                process::exit(1);
            }
            install_package(&args[2]);
        }
        // v0.08.5 fix: `mora run <file>` 子命令——之前 `run` 被当作文件名
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: mora run <file.mora>");
                process::exit(1);
            }
            run_file(&args[2], opt_level);
        }
        // v0.14/v0.15: 录制 / 重放 / 对比 / list / stats / timeline
        "record" => {
            if args.len() < 3 {
                eprintln!(
                    "Usage: mora record <file.mora> <name>\n       \
                     mora record list | stats <name> | timeline <name> | \
                     export <name> | audit <name> [--policy <file>] | report <name>"
                );
                process::exit(1);
            }
            match args[2].as_str() {
                "list" => record::run_record_list(),
                "stats" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora record stats <name>");
                        process::exit(1);
                    }
                    record::run_record_stats(&args[3]);
                }
                "timeline" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora record timeline <name>");
                        process::exit(1);
                    }
                    record::run_record_timeline(&args[3]);
                }
                "export" => {
                    if args.len() < 4 {
                        eprintln!(
                            "Usage: mora record export <name> [--format jsonl|md] [--output <file>]"
                        );
                        process::exit(1);
                    }
                    let name = &args[3];
                    let mut format = "jsonl".to_string();
                    let mut output = None;
                    let mut i = 4;
                    while i < args.len() {
                        let cur = args[i].clone();
                        match cur.as_str() {
                            "--format" | "-f" => {
                                format = flag_value(&args, &mut i, &cur, "record export");
                            }
                            "--output" | "-o" => {
                                output = Some(flag_value(&args, &mut i, &cur, "record export"));
                            }
                            other => unknown_flag(
                                other,
                                "record export",
                                &["--format", "-f", "--output", "-o"],
                            ),
                        }
                        i += 1;
                    }
                    record::run_record_export(name, &format, output.as_deref());
                }
                "audit" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora record audit <name> [--policy <file>]");
                        process::exit(1);
                    }
                    let name = &args[3];
                    let mut policy = ".moraignore".to_string();
                    let mut i = 4;
                    while i < args.len() {
                        if args[i] == "--policy" {
                            policy = flag_value(&args, &mut i, "--policy", "record audit");
                        } else if args[i].starts_with("--") {
                            // 只拦 `--x` 形态：裸词（若有）仍按位置参数放过，
                            // 与 `record export` 的「一律拒」不同 —— audit 没有
                            // 声明过任何位置参数，保守起见不扩大范围。
                            unknown_flag(&args[i], "record audit", &["--policy"]);
                        }
                        i += 1;
                    }
                    record::run_record_audit(name, &policy);
                }
                "report" => {
                    if args.len() < 4 {
                        eprintln!(
                            "Usage: mora record report <name> [--note <text>] [--verify <cmd>] [--output <file>]"
                        );
                        process::exit(1);
                    }
                    let name = &args[3];
                    let mut note = None;
                    let mut verify = None;
                    let mut output = None;
                    let mut i = 4;
                    while i < args.len() {
                        let cur = args[i].clone();
                        match cur.as_str() {
                            "--note" => {
                                note = Some(flag_value(&args, &mut i, &cur, "record report"));
                            }
                            "--verify" => {
                                verify = Some(flag_value(&args, &mut i, &cur, "record report"));
                            }
                            "--output" | "-o" => {
                                output = Some(flag_value(&args, &mut i, &cur, "record report"));
                            }
                            other => unknown_flag(
                                other,
                                "record report",
                                &["--note", "--verify", "--output", "-o"],
                            ),
                        }
                        i += 1;
                    }
                    record::run_record_report(
                        name,
                        note.as_deref(),
                        verify.as_deref(),
                        output.as_deref(),
                    );
                }
                _ => {
                    // mora record <file.mora> <name>
                    if args.len() < 4 {
                        eprintln!("Usage: mora record <file.mora> <name>");
                        process::exit(1);
                    }
                    record::run_record(&args[2], &args[3], opt_level);
                }
            }
        }
        "snapshot" => {
            if args.len() < 4 {
                eprintln!("Usage: mora snapshot <file.mora> <name> [--update]");
                process::exit(1);
            }
            let file = &args[2];
            let name = &args[3];
            // v0.104.6 D380 补漏：此前是
            // `args.iter().any(|a| a == "--update")` —— **另一种形状**，
            // 所以 D380 那轮按 `_ => {}` 计数的普查**看不到它**：
            // `mora snapshot f.mora r1 --updat`（错拼）静默忽略、exit 0。
            // 改成显式循环，未知 flag 走与其它子命令同一个报错出口。
            let mut update = false;
            for a in &args[4..] {
                // 只把 `-` 开头的当 flag：**裸词交给 D189 的
                // `reject_option_as_path` 归因**（它在更早的阶段判「选项写在了
                // 文件位置」）。这里抢着报「未知 flag」会把那条更准确的诊断盖掉。
                if !a.starts_with('-') {
                    continue;
                }
                match a.as_str() {
                    "--update" => update = true,
                    other => unknown_flag(other, "snapshot", &["--update"]),
                }
            }
            record::run_snapshot(file, name, update, opt_level);
        }
        "replay" => {
            if args.len() < 4 {
                eprintln!("Usage: mora replay <file.mora> <name>");
                process::exit(1);
            }
            record::run_replay(&args[2], &args[3], opt_level);
        }
        "diff" => {
            if args.len() < 4 {
                eprintln!("Usage: mora diff <name-a> <name-b>");
                process::exit(1);
            }
            record::run_diff(&args[2], &args[3]);
        }
        // v0.24: MCP CLI 工具
        "mcp" => {
            if args.len() < 3 {
                eprintln!("Usage: mora mcp tool-list|tool-search|toolsets");
                process::exit(1);
            }
            match args[2].as_str() {
                "tool-list" => mcp::run_mcp_tool_list(),
                "tool-search" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora mcp tool-search <query>");
                        process::exit(1);
                    }
                    mcp::run_mcp_tool_search(&args[3]);
                }
                "toolsets" => mcp::run_mcp_toolsets(),
                _ => {
                    eprintln!("Unknown mcp subcommand: {}", args[2]);
                    eprintln!("Usage: mora mcp tool-list|tool-search|toolsets");
                    process::exit(1);
                }
            }
        }
        _ => run_file(&args[1], opt_level),
    }
    0
}

fn install_package(url: &str) {
    let vendor_dir = "vendor";
    if !Path::new(vendor_dir).exists() {
        fs::create_dir(vendor_dir).expect("Failed to create vendor directory");
    }

    // Extract package name from URL
    let pkg_name = url.split('/').next_back().unwrap_or(url);
    let pkg_name = pkg_name.strip_suffix(".mora").unwrap_or(pkg_name);
    let dest = format!("{}/{}.mora", vendor_dir, pkg_name);

    println!("Installing {} from {}...", pkg_name, url);

    // Try curl first, then wget
    let result = if command_exists("curl") {
        std::process::Command::new("curl")
            .args(["-L", "-o", &dest, url])
            .output()
    } else if command_exists("wget") {
        std::process::Command::new("wget")
            .args(["-O", &dest, url])
            .output()
    } else {
        println!("Neither curl nor wget found. Please install one of them.");
        println!("Or manually download {} to {}", dest, url);
        return;
    };

    match result {
        Ok(output) => {
            if output.status.success() {
                println!("Installed {} -> {}", pkg_name, dest);
                // Update lock file
                update_lock(pkg_name, url);
            } else {
                eprintln!(
                    "Failed to download: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Err(e) => {
            eprintln!("Failed to run download command: {}", e);
        }
    }
}

fn command_exists(cmd: &str) -> bool {
    // Windows 用 where，Unix 用 which
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("where")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("which")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn print_banner() {
    use mora::config::{AI_API_KEY_ENV, AI_BASE_URL_DEFAULT, AI_BASE_URL_ENV};
    let has_openai_key = env::var(AI_API_KEY_ENV)
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    // v0.06.5: MORA_AI_MODEL 只是 ai.chat 的兜底默认；模型选择主要走
    // `with` 块的 model 绑定与 ai.chat 第二参 {model: "..."}
    let base_url = env::var(AI_BASE_URL_ENV).unwrap_or_else(|_| AI_BASE_URL_DEFAULT.to_string());

    // v0.104.6 D100：横幅走 **stderr**，不是 stdout。
    //
    // `mora run` 可以启动 **stdio JSON-RPC 服务器**（`McpServer.serve()`，
    // 也用于 LSP 形态）。这类服务器的 **stdout 就是协议通道**，
    // 而本横幅在此之前就被 `println!` 写进 stdout 且排在所有协议帧之前 ——
    // 合规客户端无法把它当帧解析。实测（`mora-lsp` 无此问题，
    // 其 stdout 直接以 `Content-Length` 开头）：
    //
    // ```text
    // Mora v0.104.5
    //   AI: mock mode (…)
    //   …共 9 行…
    //
    // Content-Length: 162
    //
    // {"id":1,"jsonrpc":"2.0",…}
    // ```
    //
    // 对普通 `mora run` 而言，横幅同样是**元数据而非程序输出**，
    // 走 stderr 才是 CLI 惯例，且不再污染 `mora run … > out.txt` 的结果。
    eprintln!("Mora v{}", mora::VERSION);
    if has_openai_key {
        eprintln!("  AI: real API (endpoint: {})", base_url);
    } else {
        eprintln!("  AI: mock mode (set OPENAI_API_KEY for real calls)");
    }
    eprintln!("  AI 原语: p\"...\" / with / stream / tool / ai.chat / AiConfig / Result<?>");
    eprintln!("  显式 API: Router::new() / McpServer::new() + route + observe / span");
    eprintln!("  Trait 系统: trait / impl / dyn / ::new() / 继承 / 默认实现");
    eprintln!("  Built-in: web.fetch / json.* / file.* / typeck (必走) / mora-lsp");
    eprintln!("  v0.15 CLI: record / replay / diff / list / stats / timeline");
    eprintln!("  ⚠  不兼容 v0.03 builtin");
    eprintln!();
}

fn update_lock(pkg_name: &str, url: &str) {
    let lock_path = "mora.lock";
    let mut content = String::new();
    if Path::new(lock_path).exists() {
        content = fs::read_to_string(lock_path).unwrap_or_default();
    }
    let entry = format!("{} = \"{}\"\n", pkg_name, url);
    if !content.contains(pkg_name) {
        content.push_str(&entry);
        fs::write(lock_path, content).expect("Failed to write lock file");
    }
}

fn run_file(path: &str, opt_level: Option<mora::mir::ssa::OptLevel>) {
    // v0.104: I/O 失败以可读错误 + 退出码 1 报告（此前 `expect` panic ——
    // 路径不存在/无权限/是目录时打印 Rust panic 与回溯，退出码 101，
    // 与 typecheck 的 exit(2)、解析错误的 exit(2) 都不一致）。
    mora::cli::reject_option_as_path(path);
    let source = match mora::cli::read_source(Path::new(path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(1);
        }
    };

    // v0.75.40: 单遍编译（compile 直接 emit MirInst + witness）
    // v0.103: 解析失败以可读错误 + 退出码 2 报告（此前 panic）。
    let (func, witnesses) = match mora::cli::compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(2);
        }
    };

    // 类型检查 (HM 推断 + 双向) — v0.75.95: 切到 _bidirectional 启用双向叠加层
    let type_errors = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        process::exit(2);
    }

    let mut interpreter = Interpreter::new();
    let mut env = interpreter.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存（run_mir + run_main_task 共享同一项）
    let func_arc = std::sync::Arc::new(func);
    if let Err(e) = mora::mir::vm::run_mir(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        eprintln!("Runtime error (MIR): {}", e);
        process::exit(1);
    }
    // 执行完顶层语句后查找并调用 main task
    if let Err(e) = mora::mir::vm::run_main_task(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        eprintln!("Runtime error (MIR main): {}", e);
        process::exit(1);
    }
}

fn run_check(path: &str) {
    mora::cli::reject_option_as_path(path);
    // v0.104: I/O 失败以可读错误 + 退出码 1 报告（此前 `expect` panic）。
    let source = match mora::cli::read_source(Path::new(path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(1);
        }
    };

    // v0.103: 解析失败以可读错误 + 退出码 2 报告（此前 `expect` panic）。
    let (_, witnesses) = match ParserV3::compile(&source) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(2);
        }
    };

    let type_errors = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if type_errors.is_empty() {
        println!("No type errors found. ({} expressions)", witnesses.len());
    } else {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        process::exit(2);
    }
}

fn run_repl() {
    let mut interpreter = Interpreter::new();
    Interpreter::run_repl_with(&mut interpreter);
}

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
    let args: Vec<String> = env::args().collect();

    // --version / --help 不显示 banner
    if args.len() >= 2 {
        match args[1].as_str() {
            "--version" | "-v" => {
                println!("Mora v{}", mora::VERSION);
                return;
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
                println!();
                println!("MCP:");
                println!("  mora mcp tool-list         List available MCP tools");
                println!("  mora mcp tool-search <q>   Search MCP tools");
                println!("  mora mcp toolsets          List available toolsets");
                println!();
                println!("  mora --version             Show version");
                println!("  mora --help                Show this help");
                return;
            }
            _ => {}
        }
    }

    // 启动横幅
    print_banner();

    if args.len() < 2 {
        run_repl();
        return;
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
                    "Usage: mora record <file.mora> <name> | mora record list|stats|timeline ..."
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
                        match args[i].as_str() {
                            "--format" | "-f" => {
                                i += 1;
                                format = args.get(i).cloned().unwrap_or(format);
                            }
                            "--output" | "-o" => {
                                i += 1;
                                output = args.get(i).cloned();
                            }
                            _ => {}
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
                        if args[i] == "--policy" && i + 1 < args.len() {
                            i += 1;
                            policy = args[i].clone();
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
                        match args[i].as_str() {
                            "--note" => {
                                i += 1;
                                note = args.get(i).cloned();
                            }
                            "--verify" => {
                                i += 1;
                                verify = args.get(i).cloned();
                            }
                            "--output" | "-o" => {
                                i += 1;
                                output = args.get(i).cloned();
                            }
                            _ => {}
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
            let update = args.iter().any(|a| a == "--update");
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
    use mora::interpreter::{AI_API_KEY_ENV, AI_BASE_URL_DEFAULT, AI_BASE_URL_ENV};
    let has_openai_key = env::var(AI_API_KEY_ENV)
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    // v0.06.5: MORA_AI_MODEL 不再作为全局默认；模型路由走 `route` 块 + `with` 块
    let base_url = env::var(AI_BASE_URL_ENV).unwrap_or_else(|_| AI_BASE_URL_DEFAULT.to_string());

    println!("Mora v{}", mora::VERSION);
    if has_openai_key {
        println!("  AI: real API (endpoint: {})", base_url);
    } else {
        println!("  AI: mock mode (set OPENAI_API_KEY for real calls)");
    }
    println!("  AI 原语: p\"...\" / with / stream / tool / ai.chat / AiConfig / Result<?>");
    println!("  显式 API: Router::new() / McpServer::new() + route + observe / span");
    println!("  Trait 系统: trait / impl / dyn / ::new() / 继承 / 默认实现");
    println!("  Built-in: web.fetch / json.* / file.* / typeck (必走) / mora-lsp");
    println!("  v0.15 CLI: record / replay / diff / list / stats / timeline");
    println!("  ⚠  不兼容 v0.03 builtin");
    println!();
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
    let source = fs::read_to_string(path).expect("Failed to read file");

    // v0.75.40: 单遍编译（compile 直接 emit MirInst + witness）
    let (func, witnesses) = mora::cli::compile_and_opt(&source, opt_level);

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
    if let Err(e) = mora::mir::vm::run_mir(&func_arc, &mut interpreter, &mut env) {
        eprintln!("Runtime error (MIR): {}", e);
        process::exit(1);
    }
    // 执行完顶层语句后查找并调用 main task
    if let Err(e) = mora::mir::vm::run_main_task(&func_arc, &mut interpreter, &mut env) {
        eprintln!("Runtime error (MIR main): {}", e);
        process::exit(1);
    }
}

fn run_check(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");

    let (_, witnesses) = ParserV3::compile(&source).expect("Failed to compile");

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

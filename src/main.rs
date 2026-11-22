//! CLI 二进制入口 — dispatch + run_file / run_check / run_repl + install / banner。
//! 共享编译与路径辅助位于 `mora::cli`，本文件保留顶层入口与命令分派。

use std::env;
use std::path::Path;
use std::process;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::typeck::format_error;

/// 编译与优化管线入口位于 `mora::cli::compile_and_opt`；
/// 本文件提供 run_file / run_check / run_repl 与 CLI dispatch。
fn main() {
    if let Some(bundle) = mora::cli::bundle::try_load_embedded_bundle() {
        let code = mora::cli::bundle::run_bundled_program(bundle);
        if code != 0 {
            process::exit(code);
        }
        return;
    }
    // v0.110.0 W2.3: Iterative Pratt expression parsing (emit_pratt_w / emit_unary_w)
    // and the heap-framed stackful VM (VmEngine) eliminate recursive expression/frame
    // OS-stack explosion, running directly on the default OS thread stack while enforcing
    // parser_v3::MAX_NESTING_DEPTH / MAX_STATEMENT_TOKENS up front.
    let code = dispatch();
    if code != 0 {
        process::exit(code);
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
                    "Mora v{} — AI-native statically-typed language for compute graphs, LLM systems, and cloud-native services",
                    mora::VERSION
                );
                println!();
                println!("Usage:");
                println!("  mora <file.mora>           Run a script");
                println!(
                    "  mora --opt=1 file.mora     Run with SSA optimization (0=off/1=basic/>=2=aggressive)"
                );
                println!("  mora --repl                Interactive REPL");
                println!("  mora --check <file>        Type check only");
                println!("  mora run <file.mora>       Run a script (explicit form)");
                println!(
                    "  mora build <file.mora>     Compile a script into an AOT MORABC02 standalone binary [--emit-bc] [-o <out>]"
                );
                println!(
                    "  mora fmt [paths...]        Format Mora source files [--check] [--stdout]"
                );
                println!(
                    "  mora test [paths...]       Discover & run *_test.mora / test_*.mora [--filter <pat>] [--jobs <N>]"
                );
                println!("  mora install <url>         Install a package from a URL");
                println!("  mora install --clean       Remove stale/broken packages from vendor/");
                println!();
                println!("  mora --version             Show version");
                println!("  mora --help                Show this help");
                return 0;
            }
            // `fmt` and `test` skip the interactive banner so stdout/stderr stay CI- and pipe-clean.
            "fmt" => {
                return mora::cli::fmt::run_fmt(&args[2..]);
            }
            "test" => {
                return mora::cli::test_runner::run_test(&args[2..], None);
            }
            // 仅看 `args[1]` 是否为 `--version` / `--help` / `fmt` / `test`；
            // 其余子命令（`run`、`build`、`install`、`--repl`、`--check`）和脚本路径继续正常分派。
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
    // 从环境变量提升为 CLI 一等参数，供 run_file 编译入口使用。
    // 未指定 → None → 走 MORA_OPT env 兜底。
    // 剥掉 flag 后重组 args，后续 match 逻辑不变（`--opt` 只在 args[1]，
    // 不进入子命令参数）。
    let opt_level: Option<mora::mir::control::OptLevel> = args
        .get(1)
        .and_then(|a| a.strip_prefix("--opt="))
        .and_then(mora::mir::control::OptLevel::from_arg);
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
                eprintln!(
                    "       mora install --clean   Remove stale/broken packages from vendor/"
                );
                process::exit(1);
            }
            // `--clean`：只清扫 vendor/ 的历史残留（旧版 install 留下的 HTTP 错误页），
            // 不安装任何东西。
            if args[2] == "--clean" {
                return mora::cli::install::clean_vendor();
            }
            // 安装失败必须以非 0 退出码结束（HTTP 4xx/5xx、传输失败、
            // 目标不可写……），否则脚本/CI 会把「装了个错误页」当成成功。
            return mora::cli::install::install_package(&args[2]);
        }
        // v0.08.5 fix: `mora run <file>` 子命令——之前 `run` 被当作文件名
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: mora run <file.mora>");
                process::exit(1);
            }
            run_file(&args[2], opt_level);
        }
        "build" => {
            run_build(&args[2..], opt_level);
        }
        "fmt" => {
            return mora::cli::fmt::run_fmt(&args[2..]);
        }
        "test" => {
            return mora::cli::test_runner::run_test(&args[2..], opt_level);
        }
        _ => run_file(&args[1], opt_level),
    }
    0
}

fn print_banner() {
    eprintln!(
        "Mora v{} — AI-native statically-typed language",
        mora::VERSION
    );
    eprintln!("  Built-in: json.* / file.* / exec.* / math.* / stats.* / linalg.* / random.*");
    eprintln!(
        "  并发与抽象: spawn / channel<T> / select / context / sleep | trait / impl / dyn / ::new()"
    );
    eprintln!(
        "  类型与执行: Span-tracked HM typeck + algebraic effects (perform/handle) + Control SSA IR / stackful VM"
    );
    eprintln!();
}

fn run_file(path: &str, opt_level: Option<mora::mir::control::OptLevel>) {
    // v0.104: I/O 失败以可读错误 + 退出码 1 报告（此前 `expect` panic ——
    // 路径不存在/无权限/是目录时打印 Rust panic 与回溯，退出码 101，
    // 与 typecheck 的 exit(2)、解析错误的 exit(2) 都不一致）。
    mora::cli::reject_option_as_path(path);
    // P2-1: Direct zero-parse execution for precompiled MORABC02 `.morac` bytecode packages.
    if let Some(code) = mora::cli::bundle::try_run_bytecode_file(Path::new(path)) {
        if code != 0 {
            process::exit(code);
        }
        return;
    }
    let source = match mora::cli::read_source(Path::new(path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(1);
        }
    };

    // W2.3: 单遍编译为 ControlProgram + witness
    let (prog, witnesses) = match mora::cli::compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(2);
        }
    };

    // 静态类型检查（单一 Span 追踪 HM 约束求解器）
    let type_errors = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        process::exit(2);
    }

    let mut core = CoreRuntime::default();
    let prog_arc = std::sync::Arc::new(prog);
    let (_top_val, globals) =
        match mora::runtime::vm::execute_program(std::sync::Arc::clone(&prog_arc), &mut core) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Runtime error (VM): {}", e);
                process::exit(1);
            }
        };
    // 执行完顶层语句后查找并调用 main task
    if let Err(e) = mora::runtime::vm::run_main_task(prog_arc, globals, &mut core) {
        eprintln!("Runtime error (VM main): {}", e);
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

    let witnesses = match ParserV3::parse_witnesses(&source) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(2);
        }
    };

    let type_errors = mora::typeck::check_mir::check_program_witnesses(&witnesses);
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
    mora::cli::repl::run_repl();
}

fn run_build(rest: &[String], opt_level: Option<mora::mir::control::OptLevel>) {
    if rest.is_empty() {
        eprintln!("Usage: mora build <file.mora> [--emit-bc] [-o <output>]");
        process::exit(1);
    }
    let mut entry: Option<&str> = None;
    let mut output: Option<&str> = None;
    let mut emit_bc = false;
    let mut i = 0usize;
    while i < rest.len() {
        let arg = &rest[i];
        if arg == "--emit-bc" || arg == "--bytecode" {
            emit_bc = true;
            i += 1;
            continue;
        }
        if arg == "-o" || arg == "--out" || arg == "--output" {
            let Some(val) = rest.get(i + 1) else {
                eprintln!("mora build: expected output path after '{arg}'");
                process::exit(1);
            };
            output = Some(val.as_str());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--output=") {
            output = Some(val);
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            eprintln!("mora build: unknown option '{arg}'");
            process::exit(2);
        }
        if entry.is_some() {
            eprintln!("mora build: unexpected extra argument '{arg}'");
            process::exit(1);
        }
        entry = Some(arg.as_str());
        i += 1;
    }
    let Some(entry_str) = entry else {
        eprintln!("Usage: mora build <file.mora> [--emit-bc] [-o <output>]");
        process::exit(1);
    };
    let entry_path = Path::new(entry_str);
    let default_out = {
        let stem = entry_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("app");
        if emit_bc {
            format!("{stem}.morac")
        } else if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    };
    let out_path = Path::new(output.unwrap_or(&default_out));
    let res = if emit_bc {
        mora::cli::bundle::build_bytecode_file(entry_path, out_path, opt_level)
    } else {
        mora::cli::bundle::build_single_binary(entry_path, out_path, opt_level)
    };
    match res {
        Ok(()) => {
            if emit_bc {
                println!("Built AOT MORABC02 bytecode -> {}", out_path.display());
            } else {
                println!(
                    "Built AOT MORABC02 standalone executable -> {}",
                    out_path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("{}: {}", entry_str, e);
            process::exit(2);
        }
    }
}

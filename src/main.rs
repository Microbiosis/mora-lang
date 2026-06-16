// mod ast; mod interpreter; ... 现在由 src/lib.rs 暴露

use std::env;
use std::fs;
use std::process;
use std::path::Path;

use mora::interpreter::Interpreter;
use mora::lexer::Lexer;
use mora::parser::Parser;
use mora::typeck;

fn main() {
    let args: Vec<String> = env::args().collect();

    // --version / --help 不显示 banner
    if args.len() >= 2 {
        match args[1].as_str() {
            "--version" | "-v" => { println!("Mora v0.04"); return; }
            "--help" | "-h" => {
                println!("Mora v0.04 — AI 原生 + 云服务原生");
                println!();
                println!("Usage:");
                println!("  mora <file.mora>        Run a script (auto-detect serve as http/mcp/repl)");
                println!("  mora --repl             Interactive REPL");
                println!("  mora --check <file>     Type check only");
                println!("  mora --version          Show version");
                println!("  mora --help             Show this help");
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
        _ => run_file(&args[1]),
    }
}

fn install_package(url: &str) {
    let vendor_dir = "vendor";
    if !Path::new(vendor_dir).exists() {
        fs::create_dir(vendor_dir).expect("Failed to create vendor directory");
    }

    // Extract package name from URL
    let pkg_name = url.split('/').last().unwrap_or(url);
    let pkg_name = pkg_name.strip_suffix(".mora").unwrap_or(pkg_name);
    let dest = format!("{}/{}.mora", vendor_dir, pkg_name);

    println!("Installing {} from {}...", pkg_name, url);

    // Try curl first, then wget
    let result = if command_exists("curl") {
        std::process::Command::new("curl")
            .args(&["-L", "-o", &dest, url])
            .output()
    } else if command_exists("wget") {
        std::process::Command::new("wget")
            .args(&["-O", &dest, url])
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
                eprintln!("Failed to download: {}", String::from_utf8_lossy(&output.stderr));
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
    let has_openai_key = env::var("OPENAI_API_KEY")
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let base_url = env::var("MORA_AI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

    println!("Mora v0.04");
    if has_openai_key {
        println!("  AI: real API (model: {}, endpoint: {})", model, base_url);
    } else {
        println!("  AI: mock mode (set OPENAI_API_KEY for real calls)");
    }
    println!("  AI 原语: p\"...\" / with / stream / tool / catch e: AiError");
    println!("  serve: http / mcp / repl / stdio + route + observe / span");
    println!("  Built-in: web.fetch / json.* / file.* / typeck / mora-lsp");
    println!("  ⚠  v0.04 不兼容 v0.03 builtin (ai.chat/stream/tool/budget/route/usage/embed/cosine/search/memory.* 均报 Unknown method)");
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

fn run_file(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.scan_tokens();

    let mut parser = Parser::new(tokens);
    let stmts = parser.parse();

    // v11: 静态类型检查（默认启用；MORA_NO_TYPECK=1 可禁用）
    if env::var("MORA_NO_TYPECK").is_err() {
        let type_errors = typeck::check_program(&stmts);
        if !type_errors.is_empty() {
            for err in &type_errors {
                if err.line > 0 {
                    eprintln!("Type error at line {}: {}", err.line, err.message);
                } else {
                    eprintln!("Type error: {}", err.message);
                }
            }
            eprintln!("\n{} type error(s) found.", type_errors.len());
            process::exit(1);
        }
    }

    let mut interpreter = Interpreter::new();
    if let Err(e) = interpreter.interpret(&stmts) {
        eprintln!("Runtime error: {}", e);
        process::exit(1);
    }
}

fn run_check(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.scan_tokens();

    let mut parser = Parser::new(tokens);
    let stmts = parser.parse();

    let type_errors = typeck::check_program(&stmts);
    if type_errors.is_empty() {
        println!("No type errors found. ({} statements)", stmts.len());
    } else {
        for err in &type_errors {
            if err.line > 0 {
                eprintln!("Type error at line {}: {}", err.line, err.message);
            } else {
                eprintln!("Type error: {}", err.message);
            }
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        process::exit(1);
    }
}

fn run_repl() {
    let mut interpreter = Interpreter::new();
    Interpreter::run_repl_with(&mut interpreter);
}

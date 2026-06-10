mod ast;
mod interpreter;
mod lexer;
mod parser;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::path::Path;

use interpreter::Interpreter;
use lexer::Lexer;
use parser::Parser;

fn main() {
    let args: Vec<String> = env::args().collect();

    // v10: 启动横幅（在 REPL/文件模式前显示）
    print_banner();

    if args.len() < 2 {
        run_repl();
        return;
    }

    match args[1].as_str() {
        "--repl" => run_repl(),
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
        println!("Or manually download {} to {}", url, dest);
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

    println!("Mora v0.01 — AI workflow scripting (real HTTP + real AI)");
    if has_openai_key {
        println!("  ai.chat / ai.create: real API (model: {}, endpoint: {})", model, base_url);
    } else {
        println!("  ai.chat: mock mode (set OPENAI_API_KEY for real calls)");
        println!("  ai.create: unavailable (requires OPENAI_API_KEY)");
    }
    println!("  web.fetch: real HTTP via ureq");
    println!("  json.parse / json.stringify: real JSON processing");
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

    let mut interpreter = Interpreter::new();
    if let Err(e) = interpreter.interpret(&stmts) {
        eprintln!("Runtime error: {}", e);
        process::exit(1);
    }
}

fn run_repl() {
    println!("Mora v0.01 REPL — type 'exit' to quit");
    println!();

    let mut interpreter = Interpreter::new();
    let stdin = io::stdin();

    loop {
        print!("mora> ");
        io::stdout().flush().unwrap();

        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() {
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            println!("Bye!");
            break;
        }

        let mut lexer = Lexer::new(line);
        let tokens = lexer.scan_tokens();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse();

        if stmts.is_empty() {
            continue;
        }

        for stmt in &stmts {
            match interpreter.execute(stmt) {
                Ok(Some(value)) => println!("= {}", value),
                Ok(None) => {}
                Err(e) => eprintln!("Error: {}", e),
            }
        }
    }
}

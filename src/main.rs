// mod ast; mod interpreter; ... 现在由 src/lib.rs 暴露

use std::env;
use std::fs;
use std::path::Path;
use std::process;

use mora::ast_v2::AstArena;
use mora::ast_v2::NodeId;
use mora::interpreter::Interpreter;
use mora::lexer::Lexer;
use mora::parser_v2::ParserV2;
use mora::record::{self, Mode};
use mora::typeck::{self, format_error};

/// 使用 ParserV2 解析代码，直接返回 v2 AST
fn parse_with_v2(source: &str) -> (Vec<NodeId>, AstArena) {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.scan_tokens();
    let mut parser_v2 = ParserV2::new(tokens);
    let node_ids = parser_v2.parse();
    let arena = parser_v2.into_arena();
    (node_ids, arena)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    // --version / --help 不显示 banner
    if args.len() >= 2 {
        match args[1].as_str() {
            "--version" | "-v" => {
                println!("Mora v0.25");
                return;
            }
            "--help" | "-h" => {
                println!(
                    "Mora v0.25 — record / replay / diff / list / stats / timeline / snapshot"
                );
                println!();
                println!("Usage:");
                println!("  mora <file.mora>           Run a script");
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
            run_file(&args[2]);
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
                "list" => run_record_list(),
                "stats" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora record stats <name>");
                        process::exit(1);
                    }
                    run_record_stats(&args[3]);
                }
                "timeline" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora record timeline <name>");
                        process::exit(1);
                    }
                    run_record_timeline(&args[3]);
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
                    run_record_export(name, &format, output.as_deref());
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
                    run_record_audit(name, &policy);
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
                    run_record_report(name, note.as_deref(), verify.as_deref(), output.as_deref());
                }
                _ => {
                    // mora record <file.mora> <name>
                    if args.len() < 4 {
                        eprintln!("Usage: mora record <file.mora> <name>");
                        process::exit(1);
                    }
                    run_record(&args[2], &args[3]);
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
            run_snapshot(file, name, update);
        }
        "replay" => {
            if args.len() < 4 {
                eprintln!("Usage: mora replay <file.mora> <name>");
                process::exit(1);
            }
            run_replay(&args[2], &args[3]);
        }
        "diff" => {
            if args.len() < 4 {
                eprintln!("Usage: mora diff <name-a> <name-b>");
                process::exit(1);
            }
            run_diff(&args[2], &args[3]);
        }
        // v0.24: MCP CLI 工具
        "mcp" => {
            if args.len() < 3 {
                eprintln!("Usage: mora mcp tool-list|tool-search|toolsets");
                process::exit(1);
            }
            match args[2].as_str() {
                "tool-list" => run_mcp_tool_list(),
                "tool-search" => {
                    if args.len() < 4 {
                        eprintln!("Usage: mora mcp tool-search <query>");
                        process::exit(1);
                    }
                    run_mcp_tool_search(&args[3]);
                }
                "toolsets" => run_mcp_toolsets(),
                _ => {
                    eprintln!("Unknown mcp subcommand: {}", args[2]);
                    eprintln!("Usage: mora mcp tool-list|tool-search|toolsets");
                    process::exit(1);
                }
            }
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
    let has_openai_key = env::var("OPENAI_API_KEY")
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    // v0.06.5: MORA_AI_MODEL 不再作为全局默认；模型路由走 `route` 块 + `with` 块
    let base_url =
        env::var("MORA_AI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

    println!("Mora v0.25");
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

fn run_file(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");

    // 使用 ParserV2 解析，直接走 v2 路径
    let (node_ids, arena) = parse_with_v2(&source);

    // v0.13: 静态类型检查必走, 无 env skip
    let type_errors = typeck::check_program(&node_ids, &arena);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        // v0.05: typeck 失败 exit code 2（区分运行时错误 1）
        process::exit(2);
    }

    let mut interpreter = Interpreter::new();
    if let Err(e) = interpreter.interpret(&node_ids, &arena) {
        eprintln!("Runtime error: {}", e);
        process::exit(1);
    }
}

// ===================================================================
// v0.14 record / replay / diff CLI
// 受 FlightBox 启发 —— AI agent 飞行记录仪
// ===================================================================

fn recordings_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("recordings");
    p
}

fn recording_path(name: &str) -> std::path::PathBuf {
    let mut p = recordings_dir();
    p.push(format!("{}.jsonl", name));
    p
}

fn run_record(path: &str, name: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("record: failed to read {}", path);
        process::exit(1);
    });

    let (node_ids, arena) = parse_with_v2(&source);

    let type_errors = typeck::check_program(&node_ids, &arena);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("record: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    interpreter.recorder = match record::Recorder::new_record(rec_path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record: {}", e);
            process::exit(1);
        }
    };

    match interpreter.interpret(&node_ids, &arena) {
        Ok(()) => {
            if let Err(e) = interpreter.recorder.save() {
                eprintln!("record: save failed: {}", e);
                process::exit(1);
            }
            let n = interpreter.recorder.events().len();
            println!("✓ recorded {} events -> {}", n, rec_path.display());
        }
        Err(e) => {
            let _ = interpreter.recorder.save();
            eprintln!("Runtime error during record: {}", e);
            eprintln!("(partial recording saved)");
            process::exit(1);
        }
    }
}

fn run_replay(path: &str, name: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("replay: failed to read {}", path);
        process::exit(1);
    });

    let (node_ids, arena) = parse_with_v2(&source);

    let type_errors = typeck::check_program(&node_ids, &arena);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("replay: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    interpreter.recorder = match record::Recorder::new_replay(rec_path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay: {}", e);
            process::exit(1);
        }
    };

    if let Err(e) = interpreter.interpret(&node_ids, &arena) {
        eprintln!("Runtime error during replay: {}", e);
        process::exit(1);
    }
    println!(
        "✓ replayed {} events from {}",
        interpreter.recorder.events().len(),
        rec_path.display()
    );
}

fn run_diff(name_a: &str, name_b: &str) {
    let rec_a = recording_path(name_a);
    let rec_b = recording_path(name_b);

    let events_a = match record::Recorder::new_replay(rec_a.clone()) {
        Ok(r) => r.events().to_vec(),
        Err(e) => {
            eprintln!("diff: {}: {}", rec_a.display(), e);
            process::exit(1);
        }
    };
    let events_b = match record::Recorder::new_replay(rec_b.clone()) {
        Ok(r) => r.events().to_vec(),
        Err(e) => {
            eprintln!("diff: {}: {}", rec_b.display(), e);
            process::exit(1);
        }
    };

    let diff = record::diff_recordings(&events_a, &events_b);
    println!(
        "diff {} ({} events)  vs  {} ({} events):",
        name_a,
        events_a.len(),
        name_b,
        events_b.len()
    );
    println!();
    for line in &diff {
        println!("{}", line.render());
    }
    let identical = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::Identical(_, _)))
        .count();
    let changed = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::Changed(_, _, _)))
        .count();
    let only_a = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::OnlyInA(_, _)))
        .count();
    let only_b = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::OnlyInB(_, _)))
        .count();
    println!();
    println!(
        "summary: identical={} changed={} only_in_{}={} only_in_{}={}",
        identical, changed, name_a, only_a, name_b, only_b
    );
}

// Hint: silence unused-import warning for Mode (used in cli dispatch later)
#[allow(dead_code)]
fn _ensure_mode_imported() -> Mode {
    Mode::Off
}

// v0.15: record list — 列出所有录制
fn run_record_list() {
    let dir = recordings_dir();
    match record::list_recordings(&dir) {
        Ok(infos) => {
            if infos.is_empty() {
                println!("No recordings found in {}", dir.display());
                return;
            }
            println!("Recordings ({}):\n", infos.len());
            println!(
                "{:<20} {:>8} {:>6} {:>20}",
                "NAME", "SIZE", "EVENTS", "LAST MODIFIED"
            );
            println!("{}", "-".repeat(60));
            for info in &infos {
                let size = format_size(info.size_bytes);
                let time = format_ts(info.last_ts_ms);
                println!(
                    "{:<20} {:>8} {:>6} {:>20}",
                    info.name, size, info.event_count, time
                );
            }
        }
        Err(e) => {
            eprintln!("record list: {}", e);
            process::exit(1);
        }
    }
}

// v0.15: record stats — 统计汇总
fn run_record_stats(name: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record stats: {}", e);
            process::exit(1);
        }
    };
    let stats = record::compute_stats(rec.events());
    println!("Recording: {}", name);
    println!("{}", "-".repeat(40));
    println!("Events:        {} total", stats.total_events);
    println!("  ai.chat:     {}", stats.ai_chat_count);
    println!("  web.fetch:   {}", stats.web_fetch_count);
    println!("  notes:       {}", stats.note_count);
    println!("Errors:        {}", stats.error_count);
    println!("{}", "-".repeat(40));
    println!(
        "Tokens:        {} in + {} out = {} total",
        stats.total_tokens_in,
        stats.total_tokens_out,
        stats.total_tokens_in + stats.total_tokens_out
    );
    if let Some(avg_in) = stats.total_tokens_in.checked_div(stats.ai_chat_count) {
        let avg_out = stats.total_tokens_out / stats.ai_chat_count;
        println!("  avg/call:    {} in + {} out", avg_in, avg_out);
    }
    println!("{}", "-".repeat(40));
    println!("Latency:       {}ms total", stats.total_latency_ms);
    if stats.ai_chat_count + stats.web_fetch_count > 0 {
        let count = stats.ai_chat_count + stats.web_fetch_count;
        println!(
            "  avg:         {}ms",
            stats.total_latency_ms / count as u128
        );
        println!("  min:         {}ms", stats.min_latency_ms);
        println!("  max:         {}ms", stats.max_latency_ms);
    }
    println!("Duration:      {}", format_duration(stats.duration_ms));
    if !stats.models.is_empty() {
        println!("Models:        {}", stats.models.join(", "));
    }
}

// v0.15: record export — 多格式导出
fn run_record_export(name: &str, format: &str, output: Option<&str>) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record export: {}", e);
            process::exit(1);
        }
    };
    let fmt = match format {
        "md" | "markdown" => record::ExportFormat::Markdown,
        _ => record::ExportFormat::Jsonl,
    };
    let content = record::export_recording(rec.events(), &fmt, name);
    match output {
        Some(out_path) => {
            if let Err(e) = fs::write(out_path, &content) {
                eprintln!("record export: failed to write {}: {}", out_path, e);
                process::exit(1);
            }
            println!("✓ exported {} events -> {}", rec.events().len(), out_path);
        }
        None => print!("{}", content),
    }
}

// v0.15: snapshot — 快照测试
fn snapshots_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("snapshots");
    p
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    let mut p = snapshots_dir();
    p.push(format!("{}.snap.jsonl", name));
    p
}

fn run_snapshot(file: &str, name: &str, update: bool) {
    let source = fs::read_to_string(file).unwrap_or_else(|_| {
        eprintln!("snapshot: failed to read {}", file);
        process::exit(1);
    });
    let (node_ids, arena) = parse_with_v2(&source);
    let type_errors = typeck::check_program(&node_ids, &arena);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("snapshot: typeck failed");
        process::exit(2);
    }
    let mut interpreter = Interpreter::new();
    if let Err(e) = interpreter.interpret(&node_ids, &arena) {
        eprintln!("snapshot: runtime error: {}", e);
        process::exit(1);
    }
    let current_events = interpreter.recorder.events().to_vec();
    let snap_file = snapshot_path(name);
    if update || !snap_file.exists() {
        // 创建/更新基线
        let snap = record::create_snapshot(name, &current_events);
        let content = record::snapshot_to_jsonl(&snap);
        let dir = snapshots_dir();
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        if let Err(e) = fs::write(&snap_file, &content) {
            eprintln!("snapshot: failed to write {}: {}", snap_file.display(), e);
            process::exit(1);
        }
        println!(
            "✓ snapshot '{}' saved ({} events)",
            name,
            snap.event_summaries.len()
        );
    } else {
        // 对比基线
        let baseline_content = fs::read_to_string(&snap_file).unwrap_or_default();
        let baseline = match record::snapshot_from_jsonl(&baseline_content) {
            Some(b) => b,
            None => {
                eprintln!("snapshot: failed to parse baseline {}", snap_file.display());
                process::exit(1);
            }
        };
        let diffs = record::diff_snapshot(&baseline, &current_events);
        let mismatches: Vec<_> = diffs
            .iter()
            .filter(|d| !matches!(d, record::SnapshotDiff::Match(_)))
            .collect();
        if mismatches.is_empty() {
            println!(
                "✓ snapshot '{}' passed ({} events match)",
                name,
                baseline.event_summaries.len()
            );
        } else {
            eprintln!(
                "✗ snapshot '{}' FAILED ({} difference(s)):\n",
                name,
                mismatches.len()
            );
            for diff in &mismatches {
                match diff {
                    record::SnapshotDiff::CountMismatch { expected, actual } => {
                        eprintln!("  event count: expected={}, actual={}", expected, actual);
                    }
                    record::SnapshotDiff::EventChanged {
                        index,
                        expected,
                        actual,
                    } => {
                        eprintln!(
                            "  #{}: expected {:?} key={}",
                            index + 1,
                            expected.kind,
                            expected.key
                        );
                        eprintln!("       got      {:?} key={}", actual.kind, actual.key);
                    }
                    record::SnapshotDiff::EventAdded { index, actual } => {
                        eprintln!(
                            "  #{}: added {:?} key={}",
                            index + 1,
                            actual.kind,
                            actual.key
                        );
                    }
                    record::SnapshotDiff::EventMissing { index, expected } => {
                        eprintln!(
                            "  #{}: missing {:?} key={}",
                            index + 1,
                            expected.kind,
                            expected.key
                        );
                    }
                    _ => {}
                }
            }
            eprintln!("\nRun with --update to regenerate baseline");
            process::exit(1);
        }
    }
}

// v0.15: record report — 红线报告
fn run_record_report(name: &str, note: Option<&str>, verify: Option<&str>, output: Option<&str>) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record report: {}", e);
            process::exit(1);
        }
    };
    let content = record::generate_report(rec.events(), name, note, verify, &[]);
    match output {
        Some(out_path) => {
            if let Err(e) = fs::write(out_path, &content) {
                eprintln!("record report: failed to write {}: {}", out_path, e);
                process::exit(1);
            }
            println!("✓ report generated -> {}", out_path);
        }
        None => print!("{}", content),
    }
}

// v0.15: record audit — 脱敏扫描
fn run_record_audit(name: &str, policy_path: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record audit: {}", e);
            process::exit(1);
        }
    };
    // 加载 .moraignore 策略
    let ignore_rules = if Path::new(policy_path).exists() {
        let content = fs::read_to_string(policy_path).unwrap_or_default();
        record::parse_moraignore(&content)
    } else {
        Vec::new()
    };
    let findings = record::audit_recording(rec.events(), &ignore_rules);
    if findings.is_empty() {
        println!("✓ No secrets found in recording '{}'", name);
        if !ignore_rules.is_empty() {
            println!(
                "  ({} rules from {} applied)",
                ignore_rules.len(),
                policy_path
            );
        }
    } else {
        println!(
            "⚠ {} potential secret(s) found in '{}':\n",
            findings.len(),
            name
        );
        println!("{:<6} {:<20} {:<20} PREVIEW", "EVENT", "FIELD", "PATTERN");
        println!("{}", "-".repeat(70));
        for f in &findings {
            println!(
                "{:<6} {:<20} {:<20} {}",
                f.event_id, f.field, f.pattern, f.preview
            );
        }
        println!(
            "\nRun with --policy {} to ignore known-safe patterns",
            policy_path
        );
        process::exit(1);
    }
}

// v0.15: record timeline — 压缩视图
fn run_record_timeline(name: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record timeline: {}", e);
            process::exit(1);
        }
    };
    let rows = record::build_timeline(rec.events());
    if rows.is_empty() {
        println!("No events in recording {}", name);
        return;
    }
    println!("Timeline: {} ({} events)\n", name, rows.len());
    println!(
        "{:<4} {:<10} {:<50} {:>10} {:>8} {:>8}",
        "#", "KIND", "DETAIL", "TOKENS", "LAT(ms)", "STATUS"
    );
    println!("{}", "-".repeat(94));
    for row in &rows {
        println!(
            "{:<4} {:<10} {:<50} {:>10} {:>8} {:>8}",
            row.seq,
            row.kind,
            truncate(&row.detail, 50),
            row.tokens,
            row.latency_ms,
            row.status
        );
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{}MB", bytes / (1024 * 1024))
    }
}

fn format_ts(ts_ms: u128) -> String {
    if ts_ms == 0 {
        return "-".to_string();
    }
    // 显示相对时间
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let diff_ms = now.saturating_sub(ts_ms);
    if diff_ms < 60_000 {
        "just now".to_string()
    } else if diff_ms < 3_600_000 {
        format!("{}min ago", diff_ms / 60_000)
    } else if diff_ms < 86_400_000 {
        format!("{}h ago", diff_ms / 3_600_000)
    } else {
        format!("{}d ago", diff_ms / 86_400_000)
    }
}

fn format_duration(ms: u128) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}min", ms as f64 / 60_000.0)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn run_check(path: &str) {
    let source = fs::read_to_string(path).expect("Failed to read file");

    let (node_ids, arena) = parse_with_v2(&source);

    let type_errors = typeck::check_program(&node_ids, &arena);
    if type_errors.is_empty() {
        println!("No type errors found. ({} statements)", node_ids.len());
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

// v0.24: MCP CLI 工具

/// 列出所有可用的 MCP 工具
fn run_mcp_tool_list() {
    use mora::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();
    let mut all_tools: Vec<(&str, &str)> = Vec::new();

    for (toolset, tools) in &toolsets {
        for tool in tools {
            all_tools.push((tool, toolset));
        }
    }

    // 去重
    all_tools.sort();
    all_tools.dedup_by(|a, b| a.0 == b.0);

    println!("MCP Tools ({}):\n", all_tools.len());
    println!("{:<30} {:<15}", "TOOL", "TOOLSET");
    println!("{}", "-".repeat(45));
    for (tool, toolset) in &all_tools {
        println!("{:<30} {:<15}", tool, toolset);
    }
}

/// 搜索 MCP 工具
fn run_mcp_tool_search(query: &str) {
    use mora::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();
    let query_lower = query.to_lowercase();
    let mut results: Vec<(&str, &str)> = Vec::new();

    for (toolset, tools) in &toolsets {
        for tool in tools {
            if tool.to_lowercase().contains(&query_lower)
                || toolset.to_lowercase().contains(&query_lower)
            {
                results.push((tool, toolset));
            }
        }
    }

    results.sort();
    results.dedup_by(|a, b| a.0 == b.0);

    if results.is_empty() {
        println!("No tools found matching '{}'", query);
    } else {
        println!("Search results for '{}' ({}):\n", query, results.len());
        println!("{:<30} {:<15}", "TOOL", "TOOLSET");
        println!("{}", "-".repeat(45));
        for (tool, toolset) in &results {
            println!("{:<30} {:<15}", tool, toolset);
        }
    }
}

/// 列出所有可用的 toolset
fn run_mcp_toolsets() {
    use mora::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();

    println!("MCP Toolsets ({}):\n", toolsets.len());
    println!("{:<15} {:>6} DESCRIPTION", "TOOLSET", "TOOLS");
    println!("{}", "-".repeat(60));
    for (toolset, tools) in &toolsets {
        let desc = match toolset.as_str() {
            "ai" => "AI 调用相关工具",
            "json" => "JSON 处理工具",
            "file" => "文件系统操作",
            "web" => "HTTP 请求工具",
            "default" => "默认启用的工具集",
            _ => "",
        };
        println!("{:<15} {:>6} {}", toolset, tools.len(), desc);
    }

    println!("\nUsage:");
    println!("  mora mcp --toolsets ai,json,file    # 启用指定 toolset");
    println!("  mora mcp --tools ai.chat,json.parse  # 启用指定工具");
    println!("  mora mcp --toolsets all              # 启用所有工具");
}

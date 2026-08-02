//! v0.75.53: record CLI 命令（从 main.rs 拆出，P9）。
//! 共享编译/路径辅助在 super::（cli/mod.rs）。

use super::*;
use crate::record;

pub fn run_record(path: &str, name: &str, opt_level: Option<crate::mir::ssa::OptLevel>) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("record: failed to read {}", path);
        process::exit(1);
    });

    let (func, witnesses) = compile_and_opt(&source, opt_level);

    let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("record: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    interpreter.infra_mut().replace_recorder(
        match record::Recorder::new_record(rec_path.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("record: {}", e);
                process::exit(1);
            }
        },
    );
    let mut env = interpreter.take_env();

    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    match crate::mir::vm::run_mir(&func_arc, &mut interpreter, &mut env) {
        Ok(_) => {
            // 执行 main task
            if let Err(e) = crate::mir::vm::run_main_task(&func_arc, &mut interpreter, &mut env) {
                if let Err(e) = interpreter.infra_mut().recorder().save() {
                    eprintln!("[warn] partial recording save failed: {}", e);
                }
                eprintln!("Runtime error during record: {}", e);
                eprintln!("(partial recording saved)");
                process::exit(1);
            }
            if let Err(e) = interpreter.infra_mut().recorder().save() {
                eprintln!("record: save failed: {}", e);
                process::exit(1);
            }
            let n = interpreter.infra().recorder().events().len();
            println!("✓ recorded {} events -> {}", n, rec_path.display());
        }
        Err(e) => {
            if let Err(e) = interpreter.infra_mut().recorder().save() {
                eprintln!("[warn] partial recording save failed: {}", e);
            }
            eprintln!("Runtime error during record: {}", e);
            eprintln!("(partial recording saved)");
            process::exit(1);
        }
    }
}

pub fn run_replay(path: &str, name: &str, opt_level: Option<crate::mir::ssa::OptLevel>) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("replay: failed to read {}", path);
        process::exit(1);
    });

    let (func, witnesses) = compile_and_opt(&source, opt_level);

    let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("replay: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    interpreter.infra_mut().replace_recorder(
        match record::Recorder::new_replay(rec_path.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("replay: {}", e);
                process::exit(1);
            }
        },
    );
    let mut env = interpreter.take_env();

    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    if let Err(e) = crate::mir::vm::run_mir(&func_arc, &mut interpreter, &mut env) {
        eprintln!("Runtime error during replay: {}", e);
        process::exit(1);
    }
    if let Err(e) = crate::mir::vm::run_main_task(&func_arc, &mut interpreter, &mut env) {
        eprintln!("Runtime error during replay main: {}", e);
        process::exit(1);
    }
    println!(
        "✓ replayed {} events from {}",
        interpreter.infra().recorder().events().len(),
        rec_path.display()
    );
}

pub fn run_diff(name_a: &str, name_b: &str) {
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

pub fn run_record_list() {
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

pub fn run_record_stats(name: &str) {
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

pub fn run_record_export(name: &str, format: &str, output: Option<&str>) {
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

pub fn run_snapshot(
    file: &str,
    name: &str,
    update: bool,
    opt_level: Option<crate::mir::ssa::OptLevel>,
) {
    let source = fs::read_to_string(file).unwrap_or_else(|_| {
        eprintln!("snapshot: failed to read {}", file);
        process::exit(1);
    });
    let (func, witnesses) = compile_and_opt(&source, opt_level);
    let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("snapshot: typeck failed");
        process::exit(2);
    }

    let mut interpreter = Interpreter::new();
    let mut env = interpreter.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    if let Err(e) = crate::mir::vm::run_mir(&func_arc, &mut interpreter, &mut env) {
        eprintln!("snapshot: runtime error: {}", e);
        process::exit(1);
    }
    if let Err(e) = crate::mir::vm::run_main_task(&func_arc, &mut interpreter, &mut env) {
        eprintln!("snapshot: runtime error: {}", e);
        process::exit(1);
    }
    let current_events = interpreter.infra().recorder().events().to_vec();
    let snap_file = snapshot_path(name);
    if update || !snap_file.exists() {
        // 创建/更新基线
        let snap = record::create_snapshot(name, &current_events);
        let content = record::snapshot_to_jsonl(&snap);
        let dir = snapshots_dir();
        if !dir.exists()
            && let Err(e) = fs::create_dir_all(&dir)
        {
            eprintln!(
                "[warn] snapshot: failed to create dir {}: {}",
                dir.display(),
                e
            );
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

pub fn run_record_report(
    name: &str,
    note: Option<&str>,
    verify: Option<&str>,
    output: Option<&str>,
) {
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

pub fn run_record_audit(name: &str, policy_path: &str) {
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

pub fn run_record_timeline(name: &str) {
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

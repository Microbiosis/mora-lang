//! P5-4B: First-class `mora profile` nanosecond profiler CLI.
//!
//! Runs a `.mora` script with the unified global [`TraceProfiler`] enabled, records spans
//! across `compute`, `gpu`, `kv_cache`, `distributed`, and `service` categories, prints a
//! formatted category latency breakdown table, and optionally exports standard Chrome Trace
//! Event JSON (`chrome://tracing` / Perfetto) via `--out <trace.json>`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::mir::control::OptLevel;
use crate::runtime::CoreRuntime;
use crate::runtime::profiler::{format_chrome_trace_json, global_handle};
use crate::runtime::vm::{execute_program, run_main_task};
use crate::typeck::format_error;

fn print_profile_usage() {
    println!("Usage: mora profile <file.mora> [--out <trace.json>] [--summary] [--opt=<0|1|2>]");
    println!();
    println!("Options:");
    println!("  --out <path>    Write Chrome Trace Event JSON (chrome://tracing / Perfetto)");
    println!(
        "  --summary       Print formatted per-category latency breakdown (default when --out omitted)"
    );
    println!("  --opt=<N>       SSA optimization level (0=off, 1=basic, 2=aggressive)");
    println!("  -h, --help      Show this help");
}

#[derive(Default)]
struct CatAgg {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

/// Entry point for `mora profile <file.mora> [--out <trace.json>] [--summary]`.
pub fn run_profile(args: &[String], initial_opt: Option<OptLevel>) -> i32 {
    let mut script_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut force_summary = false;
    let mut opt_level = initial_opt;

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                print_profile_usage();
                return 0;
            }
            "--summary" => {
                force_summary = true;
                i += 1;
            }
            "--out" | "-o" => {
                if i + 1 >= args.len() {
                    eprintln!("mora profile: --out requires a file path");
                    return 2;
                }
                out_path = Some(args[i + 1].clone());
                i += 2;
            }
            s if s.starts_with("--out=") => {
                out_path = Some(s["--out=".len()..].to_string());
                i += 1;
            }
            s if s.starts_with("--opt=") => {
                let raw = &s["--opt=".len()..];
                match OptLevel::from_arg(raw) {
                    Some(lvl) => opt_level = Some(lvl),
                    None => {
                        eprintln!(
                            "mora profile: invalid --opt value '{raw}' (expected 0, 1, or 2)"
                        );
                        return 2;
                    }
                }
                i += 1;
            }
            s if s.starts_with('-') => {
                eprintln!("mora profile: unknown option '{s}'");
                print_profile_usage();
                return 2;
            }
            path => {
                if script_path.is_some() {
                    eprintln!("mora profile: unexpected extra positional argument '{path}'");
                    return 2;
                }
                script_path = Some(path.to_string());
                i += 1;
            }
        }
    }

    let Some(path) = script_path else {
        eprintln!("mora profile: missing <file.mora> argument");
        print_profile_usage();
        return 2;
    };

    let source = match crate::cli::read_source(Path::new(&path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mora profile: {path}: {e}");
            return 1;
        }
    };

    let (prog, witnesses) = match crate::cli::compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mora profile: {path}: {e}");
            return 2;
        }
    };

    let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("\n{} type error(s) found.", type_errors.len());
        return 2;
    }

    let prof = global_handle();
    prof.start();
    let wall_start = Instant::now();
    let mut core = CoreRuntime::default();
    let prog_arc = Arc::new(prog);
    let run_res = execute_program(Arc::clone(&prog_arc), &mut core)
        .and_then(|(_top, globals)| run_main_task(prog_arc, globals, &mut core));
    let wall_elapsed = wall_start.elapsed();
    prof.stop();

    if let Err(e) = run_res {
        eprintln!("mora profile: runtime error: {e}");
        return 1;
    }

    let events = prof.snapshot();
    if let Some(ref dest) = out_path {
        let json = format_chrome_trace_json(&events);
        if let Err(e) = fs::write(dest, &json) {
            eprintln!("mora profile: failed to write trace JSON to '{dest}': {e}");
            return 1;
        }
    }

    if force_summary || out_path.is_none() || !events.is_empty() {
        let mut by_cat: BTreeMap<String, CatAgg> = BTreeMap::new();
        let mut slowest: Option<(&str, &str, u64)> = None;
        for ev in &events {
            let agg = by_cat.entry(ev.cat.clone()).or_default();
            agg.count += 1;
            agg.total_ns = agg.total_ns.saturating_add(ev.dur_ns);
            if ev.dur_ns > agg.max_ns {
                agg.max_ns = ev.dur_ns;
            }
            if slowest.is_none_or(|(_, _, d)| ev.dur_ns > d) {
                slowest = Some((ev.cat.as_str(), ev.name.as_str(), ev.dur_ns));
            }
        }

        println!(
            "Profile summary for {} (wall: {:.3} ms, {} span(s)):",
            path,
            wall_elapsed.as_secs_f64() * 1000.0,
            events.len()
        );
        if by_cat.is_empty() {
            println!(
                "  (no profiled spans recorded; use trace_dag/gpu/kv/process_group or std/trace)"
            );
        } else {
            println!(
                "  {:<14} {:>8} {:>12} {:>12} {:>12}",
                "CATEGORY", "SPANS", "TOTAL(ms)", "AVG(us)", "MAX(us)"
            );
            println!("  {}", "-".repeat(62));
            for (cat, agg) in &by_cat {
                let total_ms = agg.total_ns as f64 / 1_000_000.0;
                let avg_us = (agg.total_ns as f64 / agg.count.max(1) as f64) / 1000.0;
                let max_us = agg.max_ns as f64 / 1000.0;
                println!(
                    "  {:<14} {:>8} {:>12.3} {:>12.2} {:>12.2}",
                    cat, agg.count, total_ms, avg_us, max_us
                );
            }
            if let Some((cat, name, dur_ns)) = slowest {
                println!(
                    "  Slowest span: {cat}:{name} ({:.2} us)",
                    dur_ns as f64 / 1000.0
                );
            }
        }
        if let Some(ref dest) = out_path {
            println!("  Chrome trace written to {dest}");
        }
    }

    0
}

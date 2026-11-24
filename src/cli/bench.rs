//! P5-4B: First-class `mora bench` microbenchmark runner CLI.
//!
//! Architecture:
//! - Discovers `*_bench.mora` and `bench_*.mora` recursively under given paths (or runs explicit
//!   `.mora` files), skipping `target`, `.git`, `vendor`, etc.
//! - Compiles and HM type-checks each benchmark file once, executes top-level setup once,
//!   and discovers every zero-argument `task bench_*()` function.
//! - Runs warmup iterations followed by timed iterations (`--iters <N>`, `--warmup <W>`,
//!   `--filter <pat>`) and prints a formatted table with `min`, `p50`, `p95`, `mean`, and `ops/s`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mir::control::OptLevel;
use crate::runtime::CoreRuntime;
use crate::runtime::vm::{execute_program, list_zero_arg_bench_tasks, run_task_by_id};
use crate::typeck::format_error;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "target",
    "vendor",
    "node_modules",
];

fn print_bench_usage() {
    println!(
        "Usage: mora bench [paths...] [--filter <pattern>] [--iters <N>] [--warmup <W>] [--opt=<0|1|2>]"
    );
    println!();
    println!("Options:");
    println!("  --filter <pat>  Only run benchmarks whose name contains <pat>");
    println!("  --iters <N>     Timed iterations per benchmark (default: 10)");
    println!("  --warmup <W>    Warmup iterations before timing (default: 2)");
    println!("  --opt=<N>       SSA optimization level (0=off, 1=basic, 2=aggressive)");
    println!("  -h, --help      Show this help");
    println!();
    println!("Discovers `*_bench.mora` and `bench_*.mora` files recursively under each directory.");
    println!("Runs every zero-argument `task bench_*()` function.");
}

fn is_bench_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with("_bench.mora") || (name.starts_with("bench_") && name.ends_with(".mora"))
}

fn discover_bench_files(paths: &[String]) -> crate::error::MoraResult<Vec<PathBuf>> {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    };

    let mut out = Vec::new();
    for root in roots {
        if !root.exists() {
            return Err(crate::error::MoraError::Io(format!(
                "path not found: {}",
                root.display()
            )));
        }
        if root.is_file() {
            out.push(root);
        } else if root.is_dir() {
            walk_bench_dir(&root, &mut out)?;
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk_bench_dir(dir: &Path, out: &mut Vec<PathBuf>) -> crate::error::MoraResult<()> {
    let entries = fs::read_dir(dir).map_err(|e| {
        crate::error::MoraError::Io(format!("failed to read directory {}: {e}", dir.display()))
    })?;
    let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    children.sort();
    for p in children {
        if p.is_dir() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str())
                && SKIP_DIRS.contains(&name)
            {
                continue;
            }
            walk_bench_dir(&p, out)?;
        } else if is_bench_filename(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn display_path(path: &Path) -> String {
    let cleaned = path.strip_prefix(".").unwrap_or(path);
    cleaned.to_string_lossy().replace('\\', "/")
}

fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) as f64 * pct).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Entry point for `mora bench [paths...] [--filter <pat>] [--iters <N>] [--warmup <W>]`.
pub fn run_bench(args: &[String], initial_opt: Option<OptLevel>) -> i32 {
    let mut filter: Option<String> = None;
    let mut iters: usize = 10;
    let mut warmup: usize = 2;
    let mut opt_level = initial_opt;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                print_bench_usage();
                return 0;
            }
            "--filter" => {
                if i + 1 >= args.len() {
                    eprintln!("mora bench: --filter requires a pattern argument");
                    return 2;
                }
                filter = Some(args[i + 1].clone());
                i += 2;
            }
            s if s.starts_with("--filter=") => {
                filter = Some(s["--filter=".len()..].to_string());
                i += 1;
            }
            "--iters" | "-n" => {
                if i + 1 >= args.len() {
                    eprintln!("mora bench: --iters requires a positive integer");
                    return 2;
                }
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n > 0 => iters = n,
                    _ => {
                        eprintln!("mora bench: --iters must be a positive integer");
                        return 2;
                    }
                }
                i += 2;
            }
            s if s.starts_with("--iters=") => {
                let raw = &s["--iters=".len()..];
                match raw.parse::<usize>() {
                    Ok(n) if n > 0 => iters = n,
                    _ => {
                        eprintln!("mora bench: --iters must be a positive integer, got '{raw}'");
                        return 2;
                    }
                }
                i += 1;
            }
            "--warmup" => {
                if i + 1 >= args.len() {
                    eprintln!("mora bench: --warmup requires a non-negative integer");
                    return 2;
                }
                match args[i + 1].parse::<usize>() {
                    Ok(w) => warmup = w,
                    Err(_) => {
                        eprintln!("mora bench: --warmup must be a non-negative integer");
                        return 2;
                    }
                }
                i += 2;
            }
            s if s.starts_with("--warmup=") => {
                let raw = &s["--warmup=".len()..];
                match raw.parse::<usize>() {
                    Ok(w) => warmup = w,
                    Err(_) => {
                        eprintln!(
                            "mora bench: --warmup must be a non-negative integer, got '{raw}'"
                        );
                        return 2;
                    }
                }
                i += 1;
            }
            s if s.starts_with("--opt=") => {
                let raw = &s["--opt=".len()..];
                match OptLevel::from_arg(raw) {
                    Some(lvl) => opt_level = Some(lvl),
                    None => {
                        eprintln!("mora bench: invalid --opt value '{raw}' (expected 0, 1, or 2)");
                        return 2;
                    }
                }
                i += 1;
            }
            s if s.starts_with('-') => {
                eprintln!("mora bench: unknown option '{s}'");
                print_bench_usage();
                return 2;
            }
            p => {
                paths.push(p.to_string());
                i += 1;
            }
        }
    }

    let files = match discover_bench_files(&paths) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mora bench: {e}");
            return 1;
        }
    };
    if files.is_empty() {
        println!("mora bench: 0 benchmark files found");
        return 0;
    }

    println!(
        "Running benchmarks (warmup={warmup}, iters={iters}) across {} file(s):",
        files.len()
    );
    println!(
        "  {:<36} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "BENCHMARK", "MIN(us)", "P50(us)", "P95(us)", "MEAN(us)", "OPS/SEC"
    );
    println!("  {}", "-".repeat(94));

    let mut total_ran = 0usize;
    let mut failed = 0usize;

    for file in &files {
        let disp = display_path(file);
        let source = match crate::cli::read_source(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("FAIL {disp}: {e}");
                failed += 1;
                continue;
            }
        };
        let (prog, witnesses) = match crate::cli::compile_and_opt(&source, opt_level) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("FAIL {disp}: {e}");
                failed += 1;
                continue;
            }
        };
        let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
        if !type_errors.is_empty() {
            eprintln!("FAIL {disp}: type check failed:");
            for err in &type_errors {
                eprintln!("  {}", format_error(err));
            }
            failed += 1;
            continue;
        }

        let bench_tasks = list_zero_arg_bench_tasks(&prog);
        if bench_tasks.is_empty() {
            continue;
        }

        let prog_arc = Arc::new(prog);
        let mut core = CoreRuntime::default();
        let globals = match execute_program(Arc::clone(&prog_arc), &mut core) {
            Ok((_v, g)) => g,
            Err(e) => {
                eprintln!("FAIL {disp} (setup): {e}");
                failed += 1;
                continue;
            }
        };

        for (task_name, fn_id) in bench_tasks {
            let full_name = format!("{disp}::{task_name}");
            if let Some(ref pat) = filter
                && !full_name.contains(pat)
            {
                continue;
            }

            let mut ok = true;
            for _ in 0..warmup {
                if let Err(e) = run_task_by_id(
                    Arc::clone(&prog_arc),
                    fn_id,
                    Arc::clone(&globals),
                    &mut core,
                ) {
                    eprintln!("FAIL {full_name} (warmup): {e}");
                    failed += 1;
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }

            let mut samples = Vec::with_capacity(iters);
            for _ in 0..iters {
                let t0 = Instant::now();
                if let Err(e) = run_task_by_id(
                    Arc::clone(&prog_arc),
                    fn_id,
                    Arc::clone(&globals),
                    &mut core,
                ) {
                    eprintln!("FAIL {full_name}: {e}");
                    failed += 1;
                    ok = false;
                    break;
                }
                samples.push(t0.elapsed());
            }
            if !ok {
                continue;
            }

            samples.sort();
            let min_us = samples[0].as_secs_f64() * 1_000_000.0;
            let p50_us = percentile(&samples, 0.50).as_secs_f64() * 1_000_000.0;
            let p95_us = percentile(&samples, 0.95).as_secs_f64() * 1_000_000.0;
            let total_secs: f64 = samples.iter().map(Duration::as_secs_f64).sum();
            let mean_secs = total_secs / (samples.len().max(1) as f64);
            let mean_us = mean_secs * 1_000_000.0;
            let ops_per_sec = if mean_secs > 0.0 {
                1.0 / mean_secs
            } else {
                0.0
            };

            println!(
                "  {:<36} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>12.1}",
                full_name, min_us, p50_us, p95_us, mean_us, ops_per_sec
            );
            total_ran += 1;
        }
    }

    println!();
    println!("bench result: {total_ran} benchmark(s) completed, {failed} failed.");
    if failed > 0 { 1 } else { 0 }
}

//! v0.110.1: First-class `mora test` native test runner CLI.
//!
//! Architecture:
//! - Discovers `*_test.mora` / `test_*.mora` recursively (or executes explicit `.mora` files).
//! - Compiles via single-pass `compile_and_opt` and validates every file with the unified
//!   Span-tracked HM type checker (`check_program_witnesses`).
//! - Executes discovered zero-argument `task test_*()` functions (and top-level assertion files)
//!   concurrently on Mora's own zero-dependency [`TaskScheduler`] (`--jobs N`, `--filter <pat>`),
//!   isolating module globals per test case so parallel tests never race on top-level state.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mir::control::{ControlProgram, FnId, OptLevel};
use crate::runtime::CoreRuntime;
use crate::runtime::scheduler::{TaskContext, TaskScheduler};
use crate::runtime::vm::{execute_program, list_zero_arg_test_tasks, run_task_by_id};
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

fn print_test_usage() {
    println!("Usage: mora test [paths...] [--filter <pattern>] [--jobs <N>] [--opt=<0|1|2>]");
    println!();
    println!("Options:");
    println!("  --filter <pat>  Only run tests whose name contains <pat>");
    println!(
        "  --jobs <N>      Maximum concurrent test workers via TaskScheduler (default: CPU count)"
    );
    println!("  --opt=<N>       SSA optimization level (0=off, 1=basic, 2=aggressive)");
    println!("  -h, --help      Show this help");
    println!();
    println!("Discovers `*_test.mora` and `test_*.mora` files recursively under each directory.");
    println!("Runs top-level assertions and all zero-argument `task test_*()` functions.");
}

fn is_test_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with("_test.mora") || (name.starts_with("test_") && name.ends_with(".mora"))
}

fn discover_test_files(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    };

    let mut out = Vec::new();
    for root in roots {
        if !root.exists() {
            return Err(format!("path not found: {}", root.display()));
        }
        if root.is_file() {
            out.push(root);
        } else if root.is_dir() {
            walk_test_dir(&root, &mut out)?;
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk_test_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory {}: {e}", dir.display()))?;
    let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    children.sort();
    for p in children {
        if p.is_dir() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str())
                && SKIP_DIRS.contains(&name)
            {
                continue;
            }
            walk_test_dir(&p, out)?;
        } else if is_test_filename(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn display_test_path(path: &Path) -> String {
    let cleaned = path.strip_prefix(".").unwrap_or(path);
    cleaned.to_string_lossy().replace('\\', "/")
}

enum ScheduledCase {
    CompileFailure {
        name: String,
        error: String,
    },
    TopLevel {
        name: String,
        prog: Arc<ControlProgram>,
    },
    Task {
        name: String,
        prog: Arc<ControlProgram>,
        fn_id: FnId,
    },
}

struct CaseOutcome {
    name: String,
    elapsed: Duration,
    error: Option<String>,
}

/// Entry point for `mora test [paths...] [--filter <pat>] [--jobs <N>]`.
pub fn run_test(args: &[String], initial_opt: Option<OptLevel>) -> i32 {
    let mut filter: Option<String> = None;
    let mut jobs: Option<usize> = None;
    let mut opt_level = initial_opt;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                print_test_usage();
                return 0;
            }
            "--filter" => {
                if i + 1 >= args.len() {
                    eprintln!("mora test: --filter requires a pattern argument");
                    return 2;
                }
                filter = Some(args[i + 1].clone());
                i += 2;
            }
            s if s.starts_with("--filter=") => {
                filter = Some(s["--filter=".len()..].to_string());
                i += 1;
            }
            "--jobs" | "-j" => {
                if i + 1 >= args.len() {
                    eprintln!("mora test: --jobs requires a positive integer");
                    return 2;
                }
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => jobs = Some(n),
                    _ => {
                        eprintln!(
                            "mora test: invalid --jobs value `{}` (expected integer >= 1)",
                            args[i + 1]
                        );
                        return 2;
                    }
                }
                i += 2;
            }
            s if s.starts_with("--jobs=") => {
                let raw = &s["--jobs=".len()..];
                match raw.parse::<usize>() {
                    Ok(n) if n >= 1 => jobs = Some(n),
                    _ => {
                        eprintln!(
                            "mora test: invalid --jobs value `{raw}` (expected integer >= 1)"
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
                        eprintln!("mora test: invalid --opt value `{raw}` (expected 0, 1, or 2)");
                        return 2;
                    }
                }
                i += 1;
            }
            flag if flag.starts_with('-') => {
                eprintln!("mora test: unknown option `{flag}`. Try `mora test --help`.");
                return 2;
            }
            path => {
                paths.push(path.to_string());
                i += 1;
            }
        }
    }

    let files = match discover_test_files(&paths) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("mora test: {msg}");
            return 1;
        }
    };

    if files.is_empty() {
        println!("running 0 tests");
        println!("test result: ok. 0 passed; 0 failed; 0 filtered out");
        return 0;
    }

    let started = Instant::now();
    let mut cases: Vec<ScheduledCase> = Vec::new();
    let mut filtered_out = 0usize;

    let matches_filter = |name: &str| -> bool {
        match filter.as_deref() {
            None => true,
            Some(pat) => name.contains(pat),
        }
    };

    for path in &files {
        let rel = display_test_path(path);
        let src = match crate::cli::read_source(path) {
            Ok(s) => s,
            Err(e) => {
                if matches_filter(&rel) {
                    cases.push(ScheduledCase::CompileFailure {
                        name: rel,
                        error: format!("I/O error: {e}"),
                    });
                } else {
                    filtered_out += 1;
                }
                continue;
            }
        };

        let (prog, witnesses) = match crate::cli::compile_and_opt(&src, opt_level) {
            Ok(pair) => pair,
            Err(e) => {
                if matches_filter(&rel) {
                    cases.push(ScheduledCase::CompileFailure {
                        name: rel,
                        error: format!("compile error: {e}"),
                    });
                } else {
                    filtered_out += 1;
                }
                continue;
            }
        };

        let type_errors = crate::typeck::check_mir::check_program_witnesses(&witnesses);
        if !type_errors.is_empty() {
            if matches_filter(&rel) {
                let diag = type_errors
                    .iter()
                    .map(format_error)
                    .collect::<Vec<_>>()
                    .join("\n");
                cases.push(ScheduledCase::CompileFailure {
                    name: rel,
                    error: format!("type error(s):\n{diag}"),
                });
            } else {
                filtered_out += 1;
            }
            continue;
        }

        let prog = Arc::new(prog);
        let test_tasks = list_zero_arg_test_tasks(&prog);
        if test_tasks.is_empty() {
            let name = format!("{rel}::<top-level>");
            if matches_filter(&name) {
                cases.push(ScheduledCase::TopLevel { name, prog });
            } else {
                filtered_out += 1;
            }
        } else {
            for (task_name, fn_id) in test_tasks {
                let full_name = format!("{rel}::{task_name}");
                if matches_filter(&full_name) {
                    cases.push(ScheduledCase::Task {
                        name: full_name,
                        prog: Arc::clone(&prog),
                        fn_id,
                    });
                } else {
                    filtered_out += 1;
                }
            }
        }
    }

    let worker_count = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16)
    });

    println!(
        "running {} test(s) across {} file(s) (jobs={})",
        cases.len(),
        files.len(),
        worker_count
    );

    let scheduler = match TaskScheduler::with_worker_threads(worker_count) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mora test: failed to initialize TaskScheduler: {e}");
            return 1;
        }
    };
    let root_ctx = TaskContext::root();

    let handles: Vec<_> = cases
        .into_iter()
        .map(|case| {
            scheduler.spawn(&root_ctx, move |task_ctx| async move {
                let t0 = Instant::now();
                let outcome = match case {
                    ScheduledCase::CompileFailure { name, error } => CaseOutcome {
                        name,
                        elapsed: t0.elapsed(),
                        error: Some(error),
                    },
                    ScheduledCase::TopLevel { name, prog } => {
                        let mut core = CoreRuntime {
                            task_context: Some(task_ctx),
                            ..Default::default()
                        };
                        let err = execute_program(prog, &mut core)
                            .err()
                            .map(|e| e.to_string());
                        CaseOutcome {
                            name,
                            elapsed: t0.elapsed(),
                            error: err,
                        }
                    }
                    ScheduledCase::Task { name, prog, fn_id } => {
                        let mut core = CoreRuntime {
                            task_context: Some(task_ctx),
                            ..Default::default()
                        };
                        let err = match execute_program(Arc::clone(&prog), &mut core) {
                            Ok((_top, globals)) => run_task_by_id(prog, fn_id, globals, &mut core)
                                .err()
                                .map(|e| e.to_string()),
                            Err(e) => Some(format!("top-level init failed: {e}")),
                        };
                        CaseOutcome {
                            name,
                            elapsed: t0.elapsed(),
                            error: err,
                        }
                    }
                };
                Ok(outcome)
            })
        })
        .collect();

    let outcomes: Vec<CaseOutcome> = scheduler.block_on(async {
        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            match h.join().await {
                Ok(outcome) => results.push(outcome),
                Err(e) => results.push(CaseOutcome {
                    name: "<scheduler-task>".to_string(),
                    elapsed: Duration::ZERO,
                    error: Some(e.to_string()),
                }),
            }
        }
        results
    });

    let mut passed = 0usize;
    let mut failures: Vec<&CaseOutcome> = Vec::new();
    for out in &outcomes {
        let ms = out.elapsed.as_secs_f64() * 1000.0;
        if out.error.is_none() {
            passed += 1;
            println!("test {} ... ok ({ms:.1}ms)", out.name);
        } else {
            failures.push(out);
            println!("test {} ... FAILED ({ms:.1}ms)", out.name);
        }
    }

    if !failures.is_empty() {
        println!();
        println!("failures:");
        for f in &failures {
            println!("---- {} ----", f.name);
            if let Some(err) = &f.error {
                println!("{err}");
            }
        }
    }

    println!();
    let status = if failures.is_empty() { "ok" } else { "FAILED" };
    println!(
        "test result: {status}. {passed} passed; {} failed; {filtered_out} filtered out; finished in {:.2}s",
        failures.len(),
        started.elapsed().as_secs_f64()
    );

    if failures.is_empty() { 0 } else { 1 }
}

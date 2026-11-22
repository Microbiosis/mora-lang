//! v0.110.1: First-class `mora fmt` source formatter CLI.
//!
//! Shares the single canonical token-stream formatter (`crate::lsp::providers::formatting::format_mora_source`)
//! with `mora-lsp` so CLI formatting and editor save-on-format stay 100% identical with zero duplication.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::lsp::providers::format_mora_source;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "target",
    "vendor",
    "node_modules",
];

fn print_fmt_usage() {
    println!("Usage: mora fmt [paths...] [--check] [--stdout]");
    println!();
    println!("Options:");
    println!("  --check    Verify files are formatted; exit 1 if any file differs");
    println!("  --stdout   Print formatted output to stdout (single file or '-' for stdin)");
    println!("  -h, --help Show this help");
    println!();
    println!("When no paths are provided, recursively formats all *.mora files under '.'.");
}

fn collect_mora_files(paths: &[String]) -> crate::error::MoraResult<Vec<PathBuf>> {
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
            walk_dir(&root, &mut out)?;
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> crate::error::MoraResult<()> {
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
            walk_dir(&p, out)?;
        } else if p.extension().is_some_and(|ext| ext == "mora") {
            out.push(p);
        }
    }
    Ok(())
}

/// Entry point for `mora fmt [paths...] [--check] [--stdout]`.
pub fn run_fmt(args: &[String]) -> i32 {
    let mut check_only = false;
    let mut to_stdout = false;
    let mut paths: Vec<String> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                print_fmt_usage();
                return 0;
            }
            "--check" => check_only = true,
            "--stdout" => to_stdout = true,
            "-" => paths.push("-".to_string()),
            flag if flag.starts_with('-') => {
                eprintln!("mora fmt: unknown option `{flag}`. Try `mora fmt --help`.");
                return 2;
            }
            path => paths.push(path.to_string()),
        }
    }

    // Stdin mode (`mora fmt -` or `mora fmt --stdout -` or `mora fmt --check -`)
    if paths.len() == 1 && paths[0] == "-" {
        let mut raw = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut raw) {
            eprintln!("mora fmt: failed to read stdin: {e}");
            return 1;
        }
        let src = crate::cli::strip_bom(&raw);
        let formatted = format_mora_source(src);
        if check_only {
            if formatted != src {
                eprintln!("<stdin>");
                return 1;
            }
            return 0;
        }
        if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("mora fmt: failed to write stdout: {e}");
            return 1;
        }
        return 0;
    }

    let files = match collect_mora_files(&paths) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("mora fmt: {msg}");
            return 1;
        }
    };

    if to_stdout {
        if files.len() != 1 {
            eprintln!(
                "mora fmt: --stdout expects exactly one input file (or '-' for stdin), got {}",
                files.len()
            );
            return 2;
        }
        let path = &files[0];
        let src = match crate::cli::read_source(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mora fmt: {}: {e}", path.display());
                return 1;
            }
        };
        let formatted = format_mora_source(&src);
        if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("mora fmt: failed to write stdout: {e}");
            return 1;
        }
        return 0;
    }

    let mut unformatted: Vec<PathBuf> = Vec::new();
    let mut formatted_count = 0usize;

    for path in &files {
        let src = match crate::cli::read_source(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mora fmt: {}: {e}", path.display());
                return 1;
            }
        };
        let formatted = format_mora_source(&src);
        if formatted != src {
            if check_only {
                unformatted.push(path.clone());
            } else {
                if let Err(e) = fs::write(path, formatted.as_bytes()) {
                    eprintln!("mora fmt: failed to write {}: {e}", path.display());
                    return 1;
                }
                formatted_count += 1;
            }
        }
    }

    if check_only {
        if !unformatted.is_empty() {
            for p in &unformatted {
                println!("Diff in {}", p.display());
            }
            eprintln!(
                "mora fmt --check: {} of {} file(s) need formatting.",
                unformatted.len(),
                files.len()
            );
            return 1;
        }
        return 0;
    }

    if formatted_count > 0 {
        println!("Formatted {formatted_count} of {} file(s).", files.len());
    }
    0
}

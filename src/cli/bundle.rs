//! Wave 4 P2-1: `mora build` AOT `ControlProgram` SSA Bytecode Compilation (`MORABC02`)
//! and Self-Contained Single-Binary Execution.
//!
//! Architecture (`AGENTS.md §0`):
//! - Compiles the entry module and its entire transitive `import` closure (local `.mora` + `std/*`)
//!   through full HM type checking, SSA lowering, SSA verification, and SSA optimization at build time.
//! - Serializes ONLY verified `ControlProgram` SSA binary bytecode (`MORABC02`) — zero `.mora`
//!   source text is embedded in the produced executable or `.morac` bytecode artifact.
//! - At runtime, standalone executables and `.morac` artifacts deserialize, verify, and execute
//!   directly on the stackful VM with O(1) precompiled `import` dispatch (zero lexing, parsing,
//!   typechecking, or optimization overhead on cold start).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::cli::read_source;
use crate::error::{MoraError, MoraResult};
use crate::mir::control::codec::{
    CompiledPackage, decode_package, encode_package, is_bytecode_bytes,
};
use crate::mir::control::{OptLevel, optimize_program, verify_program};
use crate::mir::witness::{MirWitness, WitnessKind};
use crate::parser_v3::ParserV3;
use crate::runtime::CoreRuntime;
use crate::typeck::check_mir::check_program_witnesses_with_modules;
use crate::typeck::format_error;

/// Standalone executable trailer magic identifying an appended `MORABC02` bytecode payload.
pub(crate) const BUNDLE_FOOTER_MAGIC: &[u8; 12] = b"MORABC02TAIL";
const FOOTER_LEN: usize = 20; // u64 payload_len (8) + magic (12)

fn encode_opt_level(opt: Option<OptLevel>) -> u8 {
    match opt.unwrap_or_default() {
        OptLevel::None => 0,
        OptLevel::Basic => 1,
        OptLevel::Aggressive => 2,
    }
}

fn collect_imports_from_witnesses(
    witnesses: &[MirWitness],
    base_dir: Option<&Path>,
    visited: &mut HashSet<String>,
    out_modules: &mut HashMap<String, String>,
) -> MoraResult<()> {
    for w in witnesses {
        if let WitnessKind::Import(path) = &w.kind {
            if !visited.insert(path.clone()) {
                continue;
            }
            let src = if let Some(std_src) = crate::resolve_std_module(path) {
                std_src.to_string()
            } else {
                let direct = Path::new(path);
                if direct.exists() {
                    read_source(direct)?
                } else if let Some(dir) = base_dir
                    && dir.join(direct).exists()
                {
                    read_source(&dir.join(direct))?
                } else {
                    return Err(MoraError::Io(format!(
                        "mora build: cannot resolve imported module '{path}'"
                    )));
                }
            };
            out_modules.insert(path.clone(), src.clone());
            let child_w = ParserV3::parse_witnesses_with_modules(&src, Some(out_modules))?;
            collect_imports_from_witnesses(&child_w, base_dir, visited, out_modules)?;
        }
    }
    Ok(())
}

/// Compile `entry_path` and all transitive `import`s into a verified, optimized [`CompiledPackage`]
/// containing pure `ControlProgram` SSA bytecode with zero source text.
pub(crate) fn compile_package(
    entry_path: &Path,
    opt_level: Option<OptLevel>,
) -> MoraResult<CompiledPackage> {
    let entry_source = read_source(entry_path)?;
    let base_dir = entry_path.parent();
    let opt = opt_level.unwrap_or_default();

    // 1. Discover transitive import closure (local files + stdlib modules).
    let initial_w = ParserV3::parse_witnesses(&entry_source)?;
    let mut visited = HashSet::new();
    let mut source_modules = HashMap::new();
    collect_imports_from_witnesses(&initial_w, base_dir, &mut visited, &mut source_modules)?;

    let mods_opt = if source_modules.is_empty() {
        None
    } else {
        Some(&source_modules)
    };

    // 2. Compile, HM-check, optimize, and SSA-verify the entry program.
    let (mut entry_prog, entry_witnesses) =
        ParserV3::compile_with_modules(&entry_source, mods_opt)?;
    let type_errors = check_program_witnesses_with_modules(&entry_witnesses, mods_opt);
    if !type_errors.is_empty() {
        let formatted: Vec<String> = type_errors.iter().map(format_error).collect();
        return Err(MoraError::Typeck(formatted.join("\n")));
    }
    optimize_program(&mut entry_prog, opt);
    verify_program(&entry_prog)?;

    // 3. Precompile, HM-check, optimize, and SSA-verify every imported module into ControlProgram IR.
    let mut sorted_mods: Vec<(&String, &String)> = source_modules.iter().collect();
    sorted_mods.sort_by(|a, b| a.0.cmp(b.0));
    let mut compiled_modules = BTreeMap::new();
    for (mod_path, mod_src) in sorted_mods {
        let (mut mod_prog, mod_witnesses) = ParserV3::compile_with_modules(mod_src, mods_opt)?;
        let mod_errs = check_program_witnesses_with_modules(&mod_witnesses, mods_opt);
        if !mod_errs.is_empty() {
            let formatted: Vec<String> = mod_errs.iter().map(format_error).collect();
            return Err(MoraError::Typeck(format!(
                "in imported module '{mod_path}':\n{}",
                formatted.join("\n")
            )));
        }
        optimize_program(&mut mod_prog, opt);
        verify_program(&mod_prog)?;
        compiled_modules.insert(mod_path.clone(), mod_prog);
    }

    Ok(CompiledPackage {
        opt_level: encode_opt_level(opt_level),
        entry: entry_prog,
        modules: compiled_modules,
    })
}

/// Compile `entry_path` into a standalone portable `MORABC02` AOT bytecode file (`.morac`).
pub fn build_bytecode_file(
    entry_path: &Path,
    output_path: &Path,
    opt_level: Option<OptLevel>,
) -> MoraResult<()> {
    let pkg = compile_package(entry_path, opt_level)?;
    let payload = encode_package(&pkg)?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(MoraError::from)?;
    }
    fs::write(output_path, &payload).map_err(MoraError::from)?;
    Ok(())
}

/// Compile `entry_path` into verified `MORABC02` SSA bytecode (entry + transitive imports) and write
/// a standalone self-contained single-binary executable to `output_path`.
pub fn build_single_binary(
    entry_path: &Path,
    output_path: &Path,
    opt_level: Option<OptLevel>,
) -> MoraResult<()> {
    let pkg = compile_package(entry_path, opt_level)?;
    let payload = encode_package(&pkg)?;

    // Clone current mora runtime executable (stripping any existing MORABC02TAIL trailer if present).
    let exe_path = std::env::current_exe().map_err(MoraError::from)?;
    let mut exe_bytes = fs::read(&exe_path).map_err(MoraError::from)?;
    if exe_bytes.len() >= FOOTER_LEN && &exe_bytes[exe_bytes.len() - 12..] == BUNDLE_FOOTER_MAGIC {
        let len_start = exe_bytes.len() - FOOTER_LEN;
        let payload_len = u64::from_le_bytes(
            exe_bytes[len_start..len_start + 8]
                .try_into()
                .expect("8 bytes"),
        ) as usize;
        if let Some(orig_end) = len_start.checked_sub(payload_len) {
            exe_bytes.truncate(orig_end);
        }
    }

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(MoraError::from)?;
    }

    let mut out_bytes = exe_bytes;
    let payload_len = payload.len() as u64;
    out_bytes.extend_from_slice(&payload);
    out_bytes.extend_from_slice(&payload_len.to_le_bytes());
    out_bytes.extend_from_slice(BUNDLE_FOOTER_MAGIC);

    fs::write(output_path, &out_bytes).map_err(MoraError::from)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(output_path)
            .map_err(MoraError::from)?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(output_path, perms).map_err(MoraError::from)?;
    }

    Ok(())
}

/// Inspect the running executable for an appended `MORABC02` precompiled bytecode payload.
pub fn try_load_embedded_bundle() -> Option<CompiledPackage> {
    let exe_path = std::env::current_exe().ok()?;
    let mut file = fs::File::open(exe_path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < FOOTER_LEN as u64 {
        return None;
    }
    file.seek(SeekFrom::End(-(FOOTER_LEN as i64))).ok()?;
    let mut footer = [0u8; FOOTER_LEN];
    file.read_exact(&mut footer).ok()?;
    if &footer[8..20] != BUNDLE_FOOTER_MAGIC {
        return None;
    }
    let payload_len = u64::from_le_bytes(footer[0..8].try_into().ok()?);
    if payload_len + (FOOTER_LEN as u64) > file_len {
        return None;
    }
    file.seek(SeekFrom::End(-(FOOTER_LEN as i64) - (payload_len as i64)))
        .ok()?;
    let mut payload = vec![0u8; payload_len as usize];
    file.read_exact(&mut payload).ok()?;
    decode_package(&payload).ok()
}

/// If `path` is a `.morac` AOT bytecode artifact (or starts with `MORABC02` magic), load, verify,
/// and execute it directly on the stackful VM with zero source parsing.
pub fn try_run_bytecode_file(path: &Path) -> Option<i32> {
    let bytes = fs::read(path).ok()?;
    let is_morac_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("morac"));
    if !is_bytecode_bytes(&bytes) && !is_morac_ext {
        return None;
    }
    match decode_package(&bytes) {
        Ok(pkg) => Some(run_bundled_program(pkg)),
        Err(e) => {
            eprintln!("{}: {}", path.display(), e);
            Some(2)
        }
    }
}

/// Execute a precompiled [`CompiledPackage`] directly on the stackful VM with zero parsing,
/// zero HM typechecking, and O(1) precompiled `import` loading.
pub fn run_bundled_program(pkg: CompiledPackage) -> i32 {
    let precompiled: HashMap<String, Arc<crate::mir::control::ControlProgram>> = pkg
        .modules
        .into_iter()
        .map(|(k, v)| (k, Arc::new(v)))
        .collect();
    let mut core = CoreRuntime::default().with_precompiled_modules(precompiled);
    let prog_arc = Arc::new(pkg.entry);
    let (_top, globals) =
        match crate::runtime::vm::execute_program(Arc::clone(&prog_arc), &mut core) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Runtime error (VM): {e}");
                return 1;
            }
        };
    if let Err(e) = crate::runtime::vm::run_main_task(prog_arc, globals, &mut core) {
        eprintln!("Runtime error (VM main): {e}");
        return 1;
    }
    0
}

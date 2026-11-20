//! Wave 4: `mora build` single-binary compilation & self-contained bundle execution.
//!
//! Produces a standalone executable by cloning the `mora` runtime binary and appending a
//! checksum-verified `MORABUNDLE01` payload containing the verified entry source, transitive
//! `import` module closure, and SSA optimization level.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::cli::read_source;
use crate::error::{MoraError, MoraResult};
use crate::mir::control::{OptLevel, optimize_program};
use crate::mir::witness::{MirWitness, WitnessKind};
use crate::parser_v3::ParserV3;
use crate::runtime::CoreRuntime;
use crate::typeck::check_mir::check_program_witnesses_with_modules;
use crate::typeck::format_error;

const BUNDLE_MAGIC: &[u8; 12] = b"MORABUNDLE01";
const FOOTER_LEN: usize = 20; // u64 payload_len (8) + magic (12)

#[derive(Debug, Clone)]
pub struct BundledProgram {
    pub opt_level: u8,
    pub entry_source: String,
    pub modules: HashMap<String, String>,
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_opt_level(opt: Option<OptLevel>) -> u8 {
    match opt.unwrap_or_default() {
        OptLevel::None => 0,
        OptLevel::Basic => 1,
        OptLevel::Aggressive => 2,
    }
}

fn decode_opt_level(b: u8) -> OptLevel {
    match b {
        0 => OptLevel::None,
        1 => OptLevel::Basic,
        _ => OptLevel::Aggressive,
    }
}

fn serialize_bundle(bundle: &BundledProgram) -> MoraResult<Vec<u8>> {
    let mut buf = Vec::new();
    buf.push(bundle.opt_level);

    let write_str = |out: &mut Vec<u8>, s: &str| -> MoraResult<()> {
        let bytes = s.as_bytes();
        let len = u32::try_from(bytes.len()).map_err(|_| {
            MoraError::Other("mora build: bundled module exceeds 4 GiB".to_string())
        })?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
        Ok(())
    };

    write_str(&mut buf, &bundle.entry_source)?;

    let mut sorted: Vec<(&String, &String)> = bundle.modules.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let count = u32::try_from(sorted.len())
        .map_err(|_| MoraError::Other("mora build: too many bundled modules".to_string()))?;
    buf.extend_from_slice(&count.to_le_bytes());
    for (k, v) in sorted {
        write_str(&mut buf, k)?;
        write_str(&mut buf, v)?;
    }

    let csum = fnv1a_64(&buf);
    buf.extend_from_slice(&csum.to_le_bytes());
    Ok(buf)
}

fn deserialize_bundle(data: &[u8]) -> MoraResult<BundledProgram> {
    if data.len() < 13 {
        return Err(MoraError::Other(
            "corrupt mora bundle (too short)".to_string(),
        ));
    }
    let body_end = data.len() - 8;
    let expected_csum =
        u64::from_le_bytes(data[body_end..].try_into().expect("8-byte checksum slice"));
    let actual_csum = fnv1a_64(&data[..body_end]);
    if expected_csum != actual_csum {
        return Err(MoraError::Other(
            "corrupt mora bundle (checksum mismatch)".to_string(),
        ));
    }

    let body = &data[..body_end];
    let mut pos = 0usize;
    let opt_level = body[pos];
    pos += 1;

    let read_u32 = |p: &mut usize| -> MoraResult<u32> {
        if *p + 4 > body.len() {
            return Err(MoraError::Other(
                "corrupt mora bundle (truncated u32)".to_string(),
            ));
        }
        let val = u32::from_le_bytes(body[*p..*p + 4].try_into().expect("4 bytes"));
        *p += 4;
        Ok(val)
    };
    let read_str = |p: &mut usize| -> MoraResult<String> {
        let len = read_u32(p)? as usize;
        if *p + len > body.len() {
            return Err(MoraError::Other(
                "corrupt mora bundle (truncated string)".to_string(),
            ));
        }
        let s = std::str::from_utf8(&body[*p..*p + len])
            .map_err(|e| MoraError::Other(format!("corrupt mora bundle UTF-8: {e}")))?
            .to_string();
        *p += len;
        Ok(s)
    };

    let entry_source = read_str(&mut pos)?;
    let num_mods = read_u32(&mut pos)? as usize;
    let mut modules = HashMap::with_capacity(num_mods);
    for _ in 0..num_mods {
        let k = read_str(&mut pos)?;
        let v = read_str(&mut pos)?;
        modules.insert(k, v);
    }

    Ok(BundledProgram {
        opt_level,
        entry_source,
        modules,
    })
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

/// Compile `entry_path` (including transitive imports and static type checks) and write a
/// standalone single-binary executable to `output_path`.
pub fn build_single_binary(
    entry_path: &Path,
    output_path: &Path,
    opt_level: Option<OptLevel>,
) -> MoraResult<()> {
    let entry_source = read_source(entry_path)?;
    let base_dir = entry_path.parent();

    // 1. Collect transitive imports so local and stdlib modules bundle into the binary.
    let initial_w = ParserV3::parse_witnesses(&entry_source)?;
    let mut visited = HashSet::new();
    let mut modules = HashMap::new();
    collect_imports_from_witnesses(&initial_w, base_dir, &mut visited, &mut modules)?;

    // 2. Re-compile and run full HM static type checks with the bundled module table.
    let (mut prog, witnesses) = ParserV3::compile_with_modules(&entry_source, Some(&modules))?;
    let type_errors = check_program_witnesses_with_modules(&witnesses, Some(&modules));
    if !type_errors.is_empty() {
        let formatted: Vec<String> = type_errors.iter().map(format_error).collect();
        return Err(MoraError::Typeck(formatted.join("\n")));
    }
    optimize_program(&mut prog, opt_level.unwrap_or_default());

    let bundle = BundledProgram {
        opt_level: encode_opt_level(opt_level),
        entry_source,
        modules,
    };
    let payload = serialize_bundle(&bundle)?;

    // 3. Clone current mora runtime executable (stripping any existing bundle trailer if any).
    let exe_path = std::env::current_exe().map_err(MoraError::from)?;
    let mut exe_bytes = fs::read(&exe_path).map_err(MoraError::from)?;
    if exe_bytes.len() >= FOOTER_LEN && &exe_bytes[exe_bytes.len() - 12..] == BUNDLE_MAGIC {
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
    out_bytes.extend_from_slice(BUNDLE_MAGIC);

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

/// Inspect the running executable for an appended `MORABUNDLE01` payload.
pub fn try_load_embedded_bundle() -> Option<BundledProgram> {
    let exe_path = std::env::current_exe().ok()?;
    let mut file = fs::File::open(exe_path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < FOOTER_LEN as u64 {
        return None;
    }
    file.seek(SeekFrom::End(-(FOOTER_LEN as i64))).ok()?;
    let mut footer = [0u8; FOOTER_LEN];
    file.read_exact(&mut footer).ok()?;
    if &footer[8..20] != BUNDLE_MAGIC {
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
    deserialize_bundle(&payload).ok()
}

/// Execute a bundled standalone Mora program cleanly without printing CLI banners.
pub fn run_bundled_program(bundle: BundledProgram) -> i32 {
    let opt = decode_opt_level(bundle.opt_level);
    let mods_opt = if bundle.modules.is_empty() {
        None
    } else {
        Some(&bundle.modules)
    };
    let (mut prog, witnesses) = match ParserV3::compile_with_modules(&bundle.entry_source, mods_opt)
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Bundle compile error: {e}");
            return 2;
        }
    };
    let type_errs = check_program_witnesses_with_modules(&witnesses, mods_opt);
    if !type_errs.is_empty() {
        for err in &type_errs {
            eprintln!("{}", format_error(err));
        }
        return 2;
    }
    optimize_program(&mut prog, opt);

    let mut core = CoreRuntime::default().with_bundled_modules(bundle.modules);
    let prog_arc = Arc::new(prog);
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

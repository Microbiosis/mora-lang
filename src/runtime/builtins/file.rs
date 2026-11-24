//! v0.112.0: `file.*` builtin & `CheckedPath` unified async-reactor I/O engine.
//!
//! 所有路径经由 `CoreRuntime::fs_context` 校验为不可绕过的 `CheckedPath` 凭证，
//! 并直接作为一等 `Value::External(Arc<CheckedPath>)` 暴露给纯 Mora 标准库 `std/os` 与 `std/io`。
//! 所有阻塞文件系统操作通过 `CoreRuntime::reactor`（`AsyncIoReactor::run_fs`）
//! 调度到自研阻塞线程池执行，并在 `TaskContext` 取消/超时时协作中断；
//! `file.cwd` / `file.chdir` 仅维护运行时独立的 `virtual_cwd`，彻底消除进程级
//! `std::env::set_current_dir` 的并发竞态与沙箱逃逸。

use std::sync::Arc;

use super::*;
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::fs::CheckedPath;
use crate::value::Value;

fn expect_str_arg(args: &[Value], idx: usize, op: &str, name: &str) -> MoraResult<String> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(MoraError::Runtime(format!("{op}: {name} must be a string"))),
        None => Err(MoraError::Runtime(format!("{op}: missing argument {name}"))),
    }
}

fn expect_nonneg_u64_arg(args: &[Value], idx: usize, op: &str, name: &str) -> MoraResult<u64> {
    match args.get(idx) {
        Some(Value::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(Value::Int(n)) => Err(MoraError::Runtime(format!(
            "{op}: {name} must be non-negative, got {n}"
        ))),
        Some(_) => Err(MoraError::Runtime(format!("{op}: {name} must be an int"))),
        None => Err(MoraError::Runtime(format!("{op}: missing argument {name}"))),
    }
}

/// Resolve a `String` or first-class `CheckedPath` argument against `core.fs_context()`.
fn resolve_path_arg(
    core: &CoreRuntime,
    args: &[Value],
    idx: usize,
    op: &str,
    name: &str,
) -> MoraResult<(String, CheckedPath)> {
    match args.get(idx) {
        Some(Value::String(raw)) => {
            let checked = core.fs_context().resolve_path(raw, op)?;
            Ok((raw.clone(), checked))
        }
        Some(Value::External(obj)) => {
            if let Some(cp) = obj.as_any().downcast_ref::<CheckedPath>() {
                let valid = core.fs_context().validate_checked_path(cp, op)?;
                Ok((valid.display_string(), valid))
            } else {
                Err(MoraError::Runtime(format!(
                    "{op}: {name} must be a string or checked_path, got {}",
                    obj.type_name()
                )))
            }
        }
        Some(_) => Err(MoraError::Runtime(format!(
            "{op}: {name} must be a string or checked_path"
        ))),
        None => Err(MoraError::Runtime(format!("{op}: missing argument {name}"))),
    }
}

/// Execute a single-path filesystem operation directly on a verified [`CheckedPath`] capability.
/// Shared by both `file.<method>(path, ...)` and `checked_path.<method>(...)`.
pub(crate) fn call_file_method_on_checked(
    core: &mut CoreRuntime,
    checked_in: &CheckedPath,
    method: &str,
    rest_args: &[Value],
) -> MoraResult<Value> {
    let op = format!("file.{method}");
    let checked = core.fs_context().validate_checked_path(checked_in, &op)?;
    let raw = checked.display_string();

    match method {
        "read_text" => {
            let content = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::read_to_string(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot read '{raw}': {e}")))
                })?;
            Ok(Value::String(content))
        }
        "write_text" => {
            let content = expect_str_arg(rest_args, 0, &op, "content")?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let target = checked.as_path();
                    if let Some(parent) = target.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.exists()
                    {
                        return Err(MoraError::Runtime(format!(
                            "{op}: parent directory does not exist: {}",
                            parent.display()
                        )));
                    }
                    std::fs::write(target, &content)
                        .map_err(|e| MoraError::Io(format!("{op}: cannot write '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "append_text" => {
            let content = expect_str_arg(rest_args, 0, &op, "content")?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot open '{raw}': {e}")))?;
                    f.write_all(content.as_bytes())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot write '{raw}': {e}")))?;
                    Ok(())
                })?;
            Ok(Value::Nil)
        }
        "read_bytes" => {
            let bytes = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::read(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot read '{raw}': {e}")))
                })?;
            Ok(Value::String(hex_encode(&bytes)))
        }
        "write_bytes" => {
            let hex = expect_str_arg(rest_args, 0, &op, "hex")?;
            let bytes = hex_decode(&hex).map_err(|e| e.with_prefix("file.write_bytes: "))?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::write(checked.as_path(), &bytes)
                        .map_err(|e| MoraError::Io(format!("{op}: cannot write '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "read_binary" => {
            let bytes = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::read(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot read '{raw}': {e}")))
                })?;
            Ok(Value::Tensor(crate::value::tensor::Tensor::from_u8_vec(
                &bytes,
            )?))
        }
        "read_binary_range" => {
            let offset = expect_nonneg_u64_arg(rest_args, 0, &op, "offset")?;
            let length = expect_nonneg_u64_arg(rest_args, 1, &op, "len")? as usize;
            let bytes = core.reactor().run_fs(core.session.task_context.as_ref(), move || {
                use std::io::{Read, Seek, SeekFrom};
                let mut f = std::fs::File::open(checked.as_path())
                    .map_err(|e| MoraError::Io(format!("{op}: cannot open '{raw}': {e}")))?;
                let file_len = f
                    .metadata()
                    .map_err(|e| MoraError::Io(format!("{op}: cannot stat '{raw}': {e}")))?
                    .len();
                let end = offset
                    .checked_add(length as u64)
                    .ok_or_else(|| MoraError::Io(format!("{op}: offset + len overflow")))?;
                if end > file_len {
                    return Err(MoraError::Io(format!(
                        "{op}: requested range [{offset}..{end}) exceeds file size {file_len} for '{raw}'"
                    )));
                }
                f.seek(SeekFrom::Start(offset))
                    .map_err(|e| MoraError::Io(format!("{op}: seek failed on '{raw}': {e}")))?;
                let mut buf = vec![0u8; length];
                f.read_exact(&mut buf)
                    .map_err(|e| MoraError::Io(format!("{op}: read failed on '{raw}': {e}")))?;
                Ok(buf)
            })?;
            Ok(Value::Tensor(crate::value::tensor::Tensor::from_u8_vec(
                &bytes,
            )?))
        }
        "write_binary" => {
            let Some(Value::Tensor(buf_tensor)) = rest_args.first() else {
                return Err(MoraError::Runtime(
                    "file.write_binary(path, bytes): second argument must be a u8 Tensor"
                        .to_string(),
                ));
            };
            if buf_tensor.dtype != crate::common::DType::U8 {
                return Err(MoraError::Runtime(format!(
                    "file.write_binary: tensor must have dtype 'u8', got '{}'",
                    buf_tensor.dtype.short_name()
                )));
            }
            let bytes = buf_tensor.to_raw_byte_vec()?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let target = checked.as_path();
                    if let Some(parent) = target.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.exists()
                    {
                        return Err(MoraError::Runtime(format!(
                            "{op}: parent directory does not exist: {}",
                            parent.display()
                        )));
                    }
                    std::fs::write(target, &bytes)
                        .map_err(|e| MoraError::Io(format!("{op}: cannot write '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "exists" => {
            let found = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    Ok(checked.as_path().exists())
                })?;
            Ok(Value::Bool(found))
        }
        "is_file" => {
            let is_file = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    Ok(checked.as_path().is_file())
                })?;
            Ok(Value::Bool(is_file))
        }
        "is_dir" => {
            let is_dir = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    Ok(checked.as_path().is_dir())
                })?;
            Ok(Value::Bool(is_dir))
        }
        "size" => {
            let len = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let meta = std::fs::metadata(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot stat '{raw}': {e}")))?;
                    Ok(meta.len() as f64)
                })?;
            Ok(Value::Float(len))
        }
        "list" => {
            let names = core
                .reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let entries = std::fs::read_dir(checked.as_path()).map_err(|e| {
                        MoraError::Io(format!("{op}: cannot read dir '{raw}': {e}"))
                    })?;
                    let mut names: Vec<String> = Vec::new();
                    for entry in entries {
                        let entry = entry.map_err(|e| MoraError::Io(format!("{op}: {e}")))?;
                        names.push(entry.file_name().to_string_lossy().to_string());
                    }
                    names.sort();
                    Ok(names)
                })?;
            Ok(Value::List(names.into_iter().map(Value::String).collect()))
        }
        "mkdir" => {
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::create_dir(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot create '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "mkdir_all" => {
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::create_dir_all(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot create '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "remove" => {
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let p = checked.as_path();
                    if p.is_dir() {
                        std::fs::remove_dir(p).map_err(|e| {
                            MoraError::Io(format!("{op}: cannot remove dir '{raw}': {e}"))
                        })
                    } else {
                        std::fs::remove_file(p).map_err(|e| {
                            MoraError::Io(format!("{op}: cannot remove file '{raw}': {e}"))
                        })
                    }
                })?;
            Ok(Value::Nil)
        }
        "remove_all" => {
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::remove_dir_all(checked.as_path())
                        .map_err(|e| MoraError::Io(format!("{op}: cannot remove '{raw}': {e}")))
                })?;
            Ok(Value::Nil)
        }
        "touch" => {
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    let p = checked.as_path();
                    if !p.exists() {
                        if let Some(parent) = p.parent()
                            && !parent.as_os_str().is_empty()
                            && !parent.exists()
                        {
                            return Err(MoraError::Runtime(format!(
                                "{op}: parent directory does not exist: {}",
                                parent.display()
                            )));
                        }
                        std::fs::write(p, "").map_err(|e| {
                            MoraError::Io(format!("{op}: cannot create '{raw}': {e}"))
                        })?;
                    }
                    Ok(())
                })?;
            Ok(Value::Nil)
        }
        _ => Err(MoraError::Runtime(format!(
            "checked_path has no method: {method}"
        ))),
    }
}

pub fn call_file_method(core: &mut CoreRuntime, method: &str, args: &[Value]) -> MoraResult<Value> {
    let op = format!("file.{method}");

    match method {
        "check_path" => {
            let (_, checked) = resolve_path_arg(core, args, 0, &op, "path")?;
            Ok(Value::External(Arc::new(checked)))
        }
        "is_confined" => Ok(Value::Bool(core.fs_context().is_confined())),
        "env" => {
            let key = expect_str_arg(args, 0, &op, "key")?;
            match core.fs_context().env_var(&key, &op)? {
                Some(v) => Ok(Value::String(v)),
                None => Ok(Value::Nil),
            }
        }
        "os_name" => Ok(Value::String(std::env::consts::OS.to_string())),
        "arch" => Ok(Value::String(std::env::consts::ARCH.to_string())),
        "read_text" | "write_text" | "append_text" | "read_bytes" | "write_bytes"
        | "read_binary" | "read_binary_range" | "write_binary" | "exists" | "is_file"
        | "is_dir" | "size" | "list" | "mkdir" | "mkdir_all" | "remove" | "remove_all"
        | "touch" => {
            let (_, checked) = resolve_path_arg(core, args, 0, &op, "path")?;
            call_file_method_on_checked(core, &checked, method, &args[1..])
        }
        "rename" => {
            let (from, checked_from) =
                resolve_path_arg(core, args, 0, "file.rename (from)", "from")?;
            let (to, checked_to) = resolve_path_arg(core, args, 1, "file.rename (to)", "to")?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::rename(checked_from.as_path(), checked_to.as_path()).map_err(|e| {
                        MoraError::Io(format!(
                            "file.rename: cannot rename '{from}' -> '{to}': {e}"
                        ))
                    })
                })?;
            Ok(Value::Nil)
        }
        "copy" => {
            let (from, checked_from) = resolve_path_arg(core, args, 0, "file.copy (from)", "from")?;
            let (to, checked_to) = resolve_path_arg(core, args, 1, "file.copy (to)", "to")?;
            core.reactor()
                .run_fs(core.session.task_context.as_ref(), move || {
                    std::fs::copy(checked_from.as_path(), checked_to.as_path()).map_err(|e| {
                        MoraError::Io(format!("file.copy: cannot copy '{from}' -> '{to}': {e}"))
                    })?;
                    Ok(())
                })?;
            Ok(Value::Nil)
        }
        "cwd" => Ok(Value::String(
            core.fs_context().cwd().to_string_lossy().to_string(),
        )),
        "chdir" => {
            let (raw, _) = resolve_path_arg(core, args, 0, &op, "path")?;
            core.fs_context_mut().chdir(&raw, &op)?;
            Ok(Value::Nil)
        }
        "home_dir" => Ok(Value::String(core.fs_context().home_dir(&op)?)),
        "join" => {
            let mut pb = std::path::PathBuf::new();
            for arg in args {
                match arg {
                    Value::String(s) => {
                        if s.contains('\0') {
                            return Err(MoraError::Runtime(
                                "file.join: invalid NUL byte in path".to_string(),
                            ));
                        }
                        pb.push(s);
                    }
                    Value::External(obj) => {
                        if let Some(cp) = obj.as_any().downcast_ref::<CheckedPath>() {
                            pb.push(cp.as_path());
                        } else {
                            return Err(MoraError::Runtime(
                                "file.join: all arguments must be strings or checked_path"
                                    .to_string(),
                            ));
                        }
                    }
                    _ => {
                        return Err(MoraError::Runtime(
                            "file.join: all arguments must be strings or checked_path".to_string(),
                        ));
                    }
                }
            }
            Ok(Value::String(pb.to_string_lossy().to_string()))
        }
        "abs" => {
            let (_, checked) = resolve_path_arg(core, args, 0, &op, "path")?;
            Ok(Value::String(checked.display_string()))
        }
        "basename" => {
            let path = expect_str_arg(args, 0, &op, "path")?;
            let p = std::path::Path::new(&path);
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(Value::String(name))
        }
        "dirname" => {
            let path = expect_str_arg(args, 0, &op, "path")?;
            let p = std::path::Path::new(&path);
            let parent = p
                .parent()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(Value::String(parent))
        }
        "extname" => {
            let path = expect_str_arg(args, 0, &op, "path")?;
            let p = std::path::Path::new(&path);
            let ext = p
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            Ok(Value::String(ext))
        }
        _ => Err(MoraError::Runtime(format!("file.{method}: unknown method"))),
    }
}

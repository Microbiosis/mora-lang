//! v0.110.0: `file.*` builtin 实现。
//!
//! 所有路径经由 `CoreRuntime::fs_context` 校验为不可绕过的 `CheckedPath`，
//! 所有阻塞文件系统操作通过 `CoreRuntime::reactor`（`AsyncIoReactor::run_fs`）
//! 调度到自研阻塞线程池执行，并在 `TaskContext` 取消/超时时协作中断；
//! `file.cwd` / `file.chdir` 仅维护运行时独立的 `virtual_cwd`，彻底消除进程级
//! `std::env::set_current_dir` 的并发竞态与沙箱逃逸。

use super::*;
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::value::Value;

pub fn call_file_method(core: &mut CoreRuntime, method: &str, args: &[Value]) -> MoraResult<Value> {
    let expect_str = |idx: usize, name: &str| -> MoraResult<String> {
        match args.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(MoraError::Runtime(format!(
                "file.{}: {} must be a string",
                method, name
            ))),
            None => Err(MoraError::Runtime(format!(
                "file.{}: missing argument {}",
                method, name
            ))),
        }
    };
    let op = format!("file.{method}");

    match method {
        "read_text" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let content = core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::read_to_string(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.read_text: cannot read '{}': {}", raw, e))
                })
            })?;
            Ok(Value::String(content))
        }
        "write_text" => {
            let raw = expect_str(0, "path")?;
            let content = expect_str(1, "content")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                let target = checked.as_path();
                if let Some(parent) = target.parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                {
                    return Err(MoraError::Runtime(format!(
                        "file.write_text: parent directory does not exist: {}",
                        parent.display()
                    )));
                }
                std::fs::write(target, &content).map_err(|e| {
                    MoraError::Io(format!("file.write_text: cannot write '{}': {}", raw, e))
                })
            })?;
            Ok(Value::Nil)
        }
        "append_text" => {
            let raw = expect_str(0, "path")?;
            let content = expect_str(1, "content")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(checked.as_path())
                    .map_err(|e| {
                        MoraError::Io(format!("file.append_text: cannot open '{}': {}", raw, e))
                    })?;
                f.write_all(content.as_bytes()).map_err(|e| {
                    MoraError::Io(format!("file.append_text: cannot write '{}': {}", raw, e))
                })?;
                Ok(())
            })?;
            Ok(Value::Nil)
        }
        "read_bytes" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let bytes = core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::read(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.read_bytes: cannot read '{}': {}", raw, e))
                })
            })?;
            Ok(Value::String(hex_encode(&bytes)))
        }
        "write_bytes" => {
            let raw = expect_str(0, "path")?;
            let hex = expect_str(1, "hex")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let bytes = hex_decode(&hex).map_err(|e| e.with_prefix("file.write_bytes: "))?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::write(checked.as_path(), &bytes).map_err(|e| {
                    MoraError::Io(format!("file.write_bytes: cannot write '{}': {}", raw, e))
                })
            })?;
            Ok(Value::Nil)
        }
        "exists" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let found = core.reactor().run_fs(core.task_context.as_ref(), move || {
                Ok(checked.as_path().exists())
            })?;
            Ok(Value::Bool(found))
        }
        "is_file" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let is_file = core.reactor().run_fs(core.task_context.as_ref(), move || {
                Ok(checked.as_path().is_file())
            })?;
            Ok(Value::Bool(is_file))
        }
        "is_dir" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let is_dir = core.reactor().run_fs(core.task_context.as_ref(), move || {
                Ok(checked.as_path().is_dir())
            })?;
            Ok(Value::Bool(is_dir))
        }
        "size" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let len = core.reactor().run_fs(core.task_context.as_ref(), move || {
                let meta = std::fs::metadata(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.size: cannot stat '{}': {}", raw, e))
                })?;
                Ok(meta.len() as f64)
            })?;
            Ok(Value::Float(len))
        }
        "list" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            let names = core.reactor().run_fs(core.task_context.as_ref(), move || {
                let entries = std::fs::read_dir(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.list: cannot read dir '{}': {}", raw, e))
                })?;
                let mut names: Vec<String> = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| MoraError::Io(format!("file.list: {}", e)))?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();
                Ok(names)
            })?;
            Ok(Value::List(names.into_iter().map(Value::String).collect()))
        }
        "mkdir" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::create_dir(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.mkdir: cannot create '{}': {}", raw, e))
                })
            })?;
            Ok(Value::Nil)
        }
        "mkdir_all" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::create_dir_all(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.mkdir_all: cannot create '{}': {}", raw, e))
                })
            })?;
            Ok(Value::Nil)
        }
        "remove" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                let p = checked.as_path();
                if p.is_dir() {
                    std::fs::remove_dir(p).map_err(|e| {
                        MoraError::Io(format!("file.remove: cannot remove dir '{}': {}", raw, e))
                    })
                } else {
                    std::fs::remove_file(p).map_err(|e| {
                        MoraError::Io(format!("file.remove: cannot remove file '{}': {}", raw, e))
                    })
                }
            })?;
            Ok(Value::Nil)
        }
        "remove_all" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::remove_dir_all(checked.as_path()).map_err(|e| {
                    MoraError::Io(format!("file.remove_all: cannot remove '{}': {}", raw, e))
                })
            })?;
            Ok(Value::Nil)
        }
        "rename" => {
            let from = expect_str(0, "from")?;
            let to = expect_str(1, "to")?;
            let checked_from = core
                .fs_context()
                .resolve_path(&from, "file.rename (from)")?;
            let checked_to = core.fs_context().resolve_path(&to, "file.rename (to)")?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::rename(checked_from.as_path(), checked_to.as_path()).map_err(|e| {
                    MoraError::Io(format!(
                        "file.rename: cannot rename '{}' -> '{}': {}",
                        from, to, e
                    ))
                })
            })?;
            Ok(Value::Nil)
        }
        "copy" => {
            let from = expect_str(0, "from")?;
            let to = expect_str(1, "to")?;
            let checked_from = core.fs_context().resolve_path(&from, "file.copy (from)")?;
            let checked_to = core.fs_context().resolve_path(&to, "file.copy (to)")?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                std::fs::copy(checked_from.as_path(), checked_to.as_path()).map_err(|e| {
                    MoraError::Io(format!(
                        "file.copy: cannot copy '{}' -> '{}': {}",
                        from, to, e
                    ))
                })?;
                Ok(())
            })?;
            Ok(Value::Nil)
        }
        "touch" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            core.reactor().run_fs(core.task_context.as_ref(), move || {
                let p = checked.as_path();
                if !p.exists() {
                    if let Some(parent) = p.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.exists()
                    {
                        return Err(MoraError::Runtime(format!(
                            "file.touch: parent directory does not exist: {}",
                            parent.display()
                        )));
                    }
                    std::fs::write(p, "").map_err(|e| {
                        MoraError::Io(format!("file.touch: cannot create '{}': {}", raw, e))
                    })?;
                }
                Ok(())
            })?;
            Ok(Value::Nil)
        }
        "cwd" => Ok(Value::String(
            core.fs_context().cwd().to_string_lossy().to_string(),
        )),
        "chdir" => {
            let raw = expect_str(0, "path")?;
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
                    _ => {
                        return Err(MoraError::Runtime(
                            "file.join: all arguments must be strings".to_string(),
                        ));
                    }
                }
            }
            Ok(Value::String(pb.to_string_lossy().to_string()))
        }
        "abs" => {
            let raw = expect_str(0, "path")?;
            let checked = core.fs_context().resolve_path(&raw, &op)?;
            Ok(Value::String(
                checked.as_path().to_string_lossy().to_string(),
            ))
        }
        "basename" => {
            let path = expect_str(0, "path")?;
            let p = std::path::Path::new(&path);
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(Value::String(name))
        }
        "dirname" => {
            let path = expect_str(0, "path")?;
            let p = std::path::Path::new(&path);
            let parent = p
                .parent()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(Value::String(parent))
        }
        "extname" => {
            let path = expect_str(0, "path")?;
            let p = std::path::Path::new(&path);
            let ext = p
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            Ok(Value::String(ext))
        }
        _ => Err(MoraError::Runtime(format!(
            "file.{}: unknown method",
            method
        ))),
    }
}

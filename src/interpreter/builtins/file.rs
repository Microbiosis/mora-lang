//! v0.75.51: file.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_file_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let expect_str = |idx: usize, name: &str| -> Result<String, String> {
            match args.get(idx) {
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(format!("file.{}: {} must be a string", method, name)),
                None => Err(format!("file.{}: missing argument {}", method, name)),
            }
        };
        // v0.36: enforce sandbox on every path-bearing file op.
        //
        // ⚠ v0.104.6 D334：这句话此前是**假的** —— 四个带路径的入口
        // （`rename` / `copy` / `touch` / `chdir`）**漏调**本守卫。
        // `chdir` 最严重：它不自己读写沙箱外，而是**移动相对路径的解析基准**，
        // 使**其后所有** `file.*` 操作的 `check_path` 判定同时失效
        // （实测：一次 chdir 到用户可写目录后，`write_text` / `read_text` /
        // `list` 全部在沙箱外成功，详见 `chdir` 分支的注释）。
        //
        // 修法：四个入口各补本守卫。**`rename` / `copy` 要查两个路径**
        // （`from` 读、`to` 写），少查一个就能把数据搬出沙箱。
        //
        // ⇒ 新增带路径的入口时，`check_path` 不是「惯例」而是**义务**；
        // `tests/file_sandbox_coverage.rs` 逐一核对全部入口都调了它。
        let check_path = |path: &str| -> Result<(), String> {
            self.sandbox
                .sandbox
                .check_path(path)
                .map_err(|e| format!("file.{}: sandbox denied '{}': {}", method, path, e))?;
            Ok(())
        };
        match method {
            "read_text" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("file.read_text: cannot read '{}': {}", path, e))?;
                Ok(Value::String(content))
            }
            "write_text" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let content = expect_str(1, "content")?;
                if let Some(parent) = std::path::Path::new(&path).parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                {
                    return Err(format!(
                        "file.write_text: parent directory does not exist: {}",
                        parent.display()
                    ));
                }
                std::fs::write(&path, &content)
                    .map_err(|e| format!("file.write_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "append_text" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let content = expect_str(1, "content")?;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("file.append_text: cannot open '{}': {}", path, e))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| format!("file.append_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "read_bytes" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("file.read_bytes: cannot read '{}': {}", path, e))?;
                Ok(Value::String(hex_encode(&bytes)))
            }
            "write_bytes" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let hex = expect_str(1, "hex")?;
                let bytes = hex_decode(&hex).map_err(|e| format!("file.write_bytes: {}", e))?;
                std::fs::write(&path, &bytes)
                    .map_err(|e| format!("file.write_bytes: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "exists" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }
            "is_file" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                Ok(Value::Bool(std::path::Path::new(&path).is_file()))
            }
            "is_dir" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
            }
            "size" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("file.size: cannot stat '{}': {}", path, e))?;
                Ok(Value::Float(meta.len() as f64))
            }
            "list" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let entries = std::fs::read_dir(&path)
                    .map_err(|e| format!("file.list: cannot read dir '{}': {}", path, e))?;
                let mut names: Vec<String> = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| format!("file.list: {}", e))?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            "mkdir" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                std::fs::create_dir(&path)
                    .map_err(|e| format!("file.mkdir: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "mkdir_all" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                std::fs::create_dir_all(&path)
                    .map_err(|e| format!("file.mkdir_all: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "remove" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                let p = std::path::Path::new(&path);
                if p.is_dir() {
                    std::fs::remove_dir(&path)
                        .map_err(|e| format!("file.remove: cannot remove dir '{}': {}", path, e))?;
                } else {
                    std::fs::remove_file(&path).map_err(|e| {
                        format!("file.remove: cannot remove file '{}': {}", path, e)
                    })?;
                }
                Ok(Value::Nil)
            }
            "remove_all" => {
                let path = expect_str(0, "path")?;
                check_path(&path)?;
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("file.remove_all: cannot remove '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "rename" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                // v0.104.6 D334：本入口此前**没有** `check_path` ——
                // 上面那句「enforce sandbox on every path-bearing file op」
                // 的注释是假的。实测 `file.rename("C:/a.txt", "C:/b.txt")`
                // 得到的是 OS 错误「系统找不到指定的文件」，
                // 而**不是**其它入口的 `sandbox denied ... escapes fs_root`。
                //
                // `rename` 会在 fs_root 之外**移动**文件，是写操作 ⇒ 必须守卫。
                // **两个**路径都要查：`from` 是读源、`to` 是写目标，
                // 少查任一个都能被用来把数据搬出沙箱。
                check_path(&from)?;
                check_path(&to)?;
                std::fs::rename(&from, &to).map_err(|e| {
                    format!("file.rename: cannot rename '{}' -> '{}': {}", from, to, e)
                })?;
                Ok(Value::Nil)
            }
            "copy" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                // v0.104.6 D334：同 `rename`，此前**没有** `check_path`。
                // `copy` 是读源 + 写目标，两个路径都必须过守卫。
                check_path(&from)?;
                check_path(&to)?;
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("file.copy: cannot copy '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "touch" => {
                let path = expect_str(0, "path")?;
                // v0.104.6 D334：本入口此前**没有** `check_path`。`touch` 会
                // **创建**文件（`std::fs::write(&path, "")`），是写操作。
                // 实测 `file.touch("C:/x.txt")` 得到的是 OS 错误
                // 「拒绝访问」(os error 5) 而**不是** `sandbox denied`。
                check_path(&path)?;
                let p = std::path::Path::new(&path);
                if !p.exists() {
                    if let Some(parent) = p.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.exists()
                    {
                        return Err(format!(
                            "file.touch: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                    std::fs::write(&path, "")
                        .map_err(|e| format!("file.touch: cannot create '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }
            "cwd" => {
                let cwd = std::env::current_dir().map_err(|e| format!("file.cwd: {}", e))?;
                Ok(Value::String(cwd.to_string_lossy().to_string()))
            }
            "chdir" => {
                let path = expect_str(0, "path")?;
                // v0.104.6 D334：本入口此前**没有** `check_path`，且它是
                // **四个漏网入口里最严重的一个** ——
                //
                // 原因不是「chdir 自己能读写沙箱外」，而是它**移动了判定的基准**：
                // `check_path` 拿相对路径与 `fs_root` 比对，而相对路径是相对
                // **当前工作目录**解析的。所以一次 `chdir` 到沙箱外，
                // 就让**其后所有** `file.*` 操作的相对路径都从沙箱外解析。
                //
                // 实测（未修前）：
                // ```mora
                // file.chdir("C:/Users/<u>/AppData/Local/Temp")  → nil   ← 无 sandbox 拒绝
                // file.cwd()                                        → C:\Users\<u>\AppData\Local\Temp
                // file.write_text("d334_escape2.txt", "escaped")    → nil  ← **写成功**
                // file.read_text("d334_escape2.txt")                → "escaped"
                // file.list(".")                                     → 10000 项
                // ```
                // ⇒ sandbox 被**完整绕过**（该次实测在 %TEMP% 留下真实文件，
                // 已清理）。带 `check_path` 的 `write_text` / `read_text` / `list`
                // **全部**因此失效 —— 它们各自都有守卫，但守卫算出的路径已经错了。
                //
                // ⇒ 本条不是「补一个漏掉的守卫」，而是**堵住让所有守卫同时失效的入口**。
                check_path(&path)?;
                std::env::set_current_dir(&path)
                    .map_err(|e| format!("file.chdir: cannot chdir to '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "home_dir" => {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_err(|_| "file.home_dir: HOME/USERPROFILE not set".to_string())?;
                Ok(Value::String(home))
            }
            "join" => {
                let mut pb = std::path::PathBuf::new();
                for arg in args {
                    match arg {
                        Value::String(s) => pb.push(s),
                        _ => return Err("file.join: all arguments must be strings".to_string()),
                    }
                }
                Ok(Value::String(pb.to_string_lossy().to_string()))
            }
            "abs" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_err(|e| format!("file.abs: {}", e))?
                        .join(p)
                };
                Ok(Value::String(abs.to_string_lossy().to_string()))
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
            _ => Err(format!("file.{}: unknown method", method)),
        }
    }
}

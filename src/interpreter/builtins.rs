//! v0.25: 内置模块方法分发
//!
//! 从 interpreter/mod.rs 提取的内置模块方法：
//! - call_file_method: 文件操作 (read_text/write_text/exists/...)
//! - call_memory_method: 会话记忆 (store/recall/search/...)
//! - get_embedding: 向量嵌入 (mock)

use super::*;
use crate::ccr::CcrStore;
use crate::value::Value;

impl Interpreter {
    /// v0.04: file.* 内建模块 — 完整文件系统能力
    /// v0.36 (P2-3.15): every file op routes through sandbox.check_path
    /// so strict policies can block file access.
    pub fn call_file_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let expect_str = |idx: usize, name: &str| -> Result<String, String> {
            match args.get(idx) {
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(format!("file.{}: {} must be a string", method, name)),
                None => Err(format!("file.{}: missing argument {}", method, name)),
            }
        };
        // v0.36: enforce sandbox on every path-bearing file op.
        let check_path = |path: &str| -> Result<(), String> {
            self.sandbox
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
                Ok(Value::Number(meta.len() as f64))
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
                std::fs::rename(&from, &to).map_err(|e| {
                    format!("file.rename: cannot rename '{}' -> '{}': {}", from, to, e)
                })?;
                Ok(Value::Nil)
            }
            "copy" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("file.copy: cannot copy '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "touch" => {
                let path = expect_str(0, "path")?;
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

    /// v0.34: event bus.* — 事件总线 (Puter EventClient 风格 wildcard matching)
    pub fn call_event_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "emit" => {
                // v0.37 (P1-3.7): event name must be Value::String.
                let event = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("bus.emit: first arg must be a string event name".to_string())
                    }
                    None => return Err("bus.emit: requires event name as first arg".to_string()),
                };
                let payload = args.get(1).cloned().unwrap_or(Value::Nil);
                self.bus.emit(&event, &payload);
                Ok(Value::Nil)
            }
            "off" => {
                // v0.37: pattern must be Value::String.
                let pattern = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("bus.off: first arg must be a string pattern".to_string())
                    }
                    None => return Err("bus.off: requires pattern as first arg".to_string()),
                };
                self.bus.off(&pattern);
                Ok(Value::Nil)
            }
            "count" => Ok(Value::Number(self.bus.pattern_count() as f64)),
            _ => Err(format!("bus.{}: unknown method", method)),
        }
    }

    /// v0.34: sandbox.* — path validation + builtin allow/deny (MimiClaw + AIOS)
    pub fn call_sandbox_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "mode" => {
                let policy = &self.sandbox;
                let mode = if policy.allow.iter().any(|p| p == "*") && policy.deny.is_empty() {
                    "permissive"
                } else if policy.allow.is_empty() {
                    "strict"
                } else {
                    "custom"
                };
                Ok(Value::String(mode.to_string()))
            }
            "check_builtin" => {
                // v0.37: builtin name must be Value::String.
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err(
                            "sandbox.check_builtin: name must be a string".to_string(),
                        );
                    }
                    None => {
                        return Err(
                            "sandbox.check_builtin: requires builtin name as first arg".to_string(),
                        );
                    }
                };
                Ok(Value::Bool(self.sandbox.check_builtin(&name).is_ok()))
            }
            "check_path" => {
                // v0.37: path must be Value::String.
                let path = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err(
                            "sandbox.check_path: path must be a string".to_string(),
                        );
                    }
                    None => {
                        return Err(
                            "sandbox.check_path: requires path as first arg".to_string(),
                        );
                    }
                };
                Ok(Value::Bool(self.sandbox.check_path(&path).is_ok()))
            }
            _ => Err(format!("sandbox.{}: unknown method", method)),
        }
    }

    /// v0.34: schedule.* — cron scheduler (MimiClaw style)
    pub fn call_schedule_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "add" => {
                // v0.37 (P1-3.9): name/kind/message must all be Value::String.
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("schedule.add: name must be a string".to_string()),
                    None => return Err("schedule.add: requires name".to_string()),
                };
                let kind_str = match args.get(1) {
                    Some(Value::String(s)) => s.as_str(),
                    Some(_) => {
                        return Err("schedule.add: kind must be a string".to_string());
                    }
                    None => {
                        return Err("schedule.add: requires kind ('every' or 'at')".to_string());
                    }
                };
                let kind = match kind_str {
                    "every" => crate::schedule::JobKind::Every,
                    "at" => crate::schedule::JobKind::At,
                    _ => {
                        return Err(format!(
                            "schedule.add: kind must be 'every' or 'at', got '{}'",
                            kind_str
                        ));
                    }
                };
                let message = match args.get(2) {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("schedule.add: message must be a string".to_string());
                    }
                    None => return Err("schedule.add: requires message".to_string()),
                };
                let interval_s = if let Some(Value::Number(n)) = args.get(3) {
                    *n as u64
                } else {
                    0
                };
                let at_epoch = if let Some(Value::Number(n)) = args.get(4) {
                    *n as u64
                } else {
                    0
                };
                self.scheduler
                    .add(&name, kind, &message, interval_s, at_epoch)
                    .map(Value::String)
            }
            "list" => {
                let jobs = self.scheduler.list();
                let arr: Vec<Value> = jobs
                    .into_iter()
                    .map(|j| {
                        let mut m = std::collections::HashMap::new();
                        m.insert("id".to_string(), Value::String(j.id));
                        m.insert("name".to_string(), Value::String(j.name));
                        m.insert(
                            "kind".to_string(),
                            Value::String(match j.kind {
                                crate::schedule::JobKind::Every => "every".to_string(),
                                crate::schedule::JobKind::At => "at".to_string(),
                            }),
                        );
                        m.insert("message".to_string(), Value::String(j.message));
                        m.insert("interval_s".to_string(), Value::Number(j.interval_s as f64));
                        m.insert("at_epoch".to_string(), Value::Number(j.at_epoch as f64));
                        Value::Dict(m)
                    })
                    .collect();
                Ok(Value::List(arr))
            }
            "remove" => {
                let id = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("schedule.remove: requires id")?;
                Ok(Value::Bool(self.scheduler.remove(&id)))
            }
            "tick" => {
                let messages = self.scheduler.tick(crate::schedule::Scheduler::now());
                Ok(Value::List(
                    messages.into_iter().map(Value::String).collect(),
                ))
            }
            "count" => Ok(Value::Number(self.scheduler.count() as f64)),
            _ => Err(format!("schedule.{}: unknown method", method)),
        }
    }

    /// v0.34: ai.tokens — expose TokenUsage counters (mini-swe-agent cost tracking pattern)
    pub fn call_ai_tokens_method(&self, method: &str, _args: &[Value]) -> Result<Value, String> {
        match method {
            "input" => Ok(Value::Number(self.token_usage.input as f64)),
            "output" => Ok(Value::Number(self.token_usage.output as f64)),
            "total" => Ok(Value::Number(
                (self.token_usage.input + self.token_usage.output) as f64,
            )),
            "calls" => Ok(Value::Number(self.token_usage.input as f64)),
            _ => Err(format!("ai.tokens.{}: unknown method", method)),
        }
    }

    /// v0.34: ccr.* — Compress-Cache-Retrieve (Headroom style)
    pub fn call_ccr_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "put" => {
                // v0.37 (P1-3.8): data must be Value::String. Avoids lossy
                // to_string() of List/Dict that would round-trip into "[...]".
                let data = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("ccr.put: data must be a string".to_string());
                    }
                    None => return Err("ccr.put: requires data as first arg".to_string()),
                };
                let hash = self.ccr_store.put(&data);
                Ok(Value::String(hash))
            }
            "get" => {
                let hash = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("ccr.get: hash must be a string".to_string());
                    }
                    None => return Err("ccr.get: requires hash as first arg".to_string()),
                };
                match self.ccr_store.get(&hash) {
                    Some(entry) => Ok(Value::String(entry.data)),
                    None => Ok(Value::Nil),
                }
            }
            "len" => Ok(Value::Number(self.ccr_store.len() as f64)),
            "marker" => {
                let hash = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ccr.marker: requires hash as first arg")?;
                let size = if let Some(Value::Number(n)) = args.get(1) {
                    *n as usize
                } else {
                    0
                };
                Ok(Value::String(crate::ccr::make_marker(&hash, size)))
            }
            "extract" => {
                let marker = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ccr.extract: requires marker as first arg")?;
                match crate::ccr::extract_hash(&marker) {
                    Some(hash) => Ok(Value::String(hash.to_string())),
                    None => Err(format!("ccr.extract: not a valid CCR marker: '{}'", marker)),
                }
            }
            _ => Err(format!("ccr.{}: unknown method", method)),
        }
    }

    /// v0.34: mock.* — mock registry (OpenFugu + OpenInfer mock)
    pub fn call_mock_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "register" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("mock.register: name must be a string".to_string());
                    }
                    None => return Err("mock.register: requires name".to_string()),
                };
                let handler = args
                    .get(1)
                    .cloned()
                    .ok_or("mock.register: requires handler")?;
                self.mock_registry
                    .register(&name, crate::mock::MockHandler::Script(handler));
                Ok(Value::String(format!("mock.{} registered", name)))
            }
            "unregister" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("mock.unregister: name must be a string".to_string());
                    }
                    None => return Err("mock.unregister: requires name".to_string()),
                };
                self.mock_registry.unregister(&name);
                Ok(Value::Nil)
            }
            "call" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("mock.call: name must be a string".to_string()),
                    None => return Err("mock.call: requires name".to_string()),
                };
                let call_args = args.get(1).cloned().unwrap_or(Value::Nil);
                match self.mock_registry.get(&name) {
                    Some(crate::mock::MockHandler::Native(f)) => Ok(f(&call_args)),
                    Some(crate::mock::MockHandler::Script(closure)) => {
                        self.call_value(&closure, vec![call_args])
                    }
                    None => Ok(Value::Nil),
                }
            }
            "count" => Ok(Value::Number(self.mock_registry.count() as f64)),
            "names" => {
                let names = self.mock_registry.names();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            _ => Err(format!("mock.{}: unknown method", method)),
        }
    }

    /// v0.25: memory.* — 会话记忆系统
    pub fn call_memory_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "store" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.store: requires key")?;
                let value = args.get(1).cloned().unwrap_or(Value::Nil);
                self.memory_store.insert(key, value);
                Ok(Value::Nil)
            }
            "recall" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.recall: requires key")?;
                Ok(self.memory_store.get(&key).cloned().unwrap_or(Value::Nil))
            }
            "search" => {
                let query = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.search: requires query")?;
                let query_lower = query.to_lowercase();
                let results: Vec<Value> = self
                    .memory_store
                    .iter()
                    .filter(|(k, _)| k.to_lowercase().contains(&query_lower))
                    .map(|(k, v)| {
                        let mut m = HashMap::new();
                        m.insert("key".to_string(), Value::String(k.clone()));
                        m.insert("value".to_string(), v.clone());
                        Value::Dict(m)
                    })
                    .collect();
                Ok(Value::List(results))
            }
            "forget" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.forget: requires key")?;
                self.memory_store.remove(&key);
                Ok(Value::Nil)
            }
            "clear" => {
                self.memory_store.clear();
                Ok(Value::Nil)
            }
            "size" => Ok(Value::Number(self.memory_store.len() as f64)),
            "keys" => {
                let keys: Vec<Value> = self
                    .memory_store
                    .keys()
                    .map(|k| Value::String(k.clone()))
                    .collect();
                Ok(Value::List(keys))
            }
            "save" => {
                let path = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.save: requires path")?;
                let json = value_to_json(&Value::Dict(self.memory_store.clone()));
                fs::write(&path, json).map_err(|e| format!("memory.save: {}", e))?;
                Ok(Value::Bool(true))
            }
            "load" => {
                let path = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.load: requires path")?;
                let content =
                    fs::read_to_string(&path).map_err(|e| format!("memory.load: {}", e))?;
                match json_to_value(&content) {
                    Ok(Value::Dict(map)) => {
                        self.memory_store = map;
                        Ok(Value::Bool(true))
                    }
                    Ok(_) => Err("memory.load: file must contain a JSON object".to_string()),
                    Err(e) => Err(format!("memory.load: {}", e)),
                }
            }
            _ => Err(format!("memory has no method: {}", method)),
        }
    }

    #[allow(dead_code)]
    pub fn get_embedding(&self, text: &str) -> Result<Vec<f64>, String> {
        Ok(mock_bow_embedding(text))
    }
}

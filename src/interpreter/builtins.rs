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
                        return Err("bus.emit: first arg must be a string event name".to_string());
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
                        return Err("bus.off: first arg must be a string pattern".to_string());
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
                        return Err("sandbox.check_builtin: name must be a string".to_string());
                    }
                    None => {
                        return Err(
                            "sandbox.check_builtin: requires builtin name as first arg".to_string()
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
                        return Err("sandbox.check_path: path must be a string".to_string());
                    }
                    None => {
                        return Err("sandbox.check_path: requires path as first arg".to_string());
                    }
                };
                Ok(Value::Bool(self.sandbox.check_path(&path).is_ok()))
            }
            // v0.42.0: sandbox.key { file.read, web.fetch } — issue capability token
            // Returns: token handle as Value::Number(token_id)
            "key" => {
                use std::collections::BTreeSet;
                use std::time::Duration;

                let mut allowed = BTreeSet::new();
                for arg in args {
                    match arg {
                        Value::String(s) => {
                            let cap = crate::sandbox::Capability::parse(s).ok_or_else(|| {
                                format!("sandbox.key: unknown capability '{}'", s)
                            })?;
                            allowed.insert(cap);
                        }
                        _ => {
                            return Err(
                                "sandbox.key: all args must be capability strings (e.g. \"file.read\")"
                                    .to_string(),
                            );
                        }
                    }
                }
                // v0.42.0: 无 TTL (None = 永不过期); 后续可加 sandbox.key_ttl { ... }
                let ttl: Option<Duration> = None;
                let token_id = self
                    .sandbox
                    .capabilities
                    .issue(allowed, ttl)
                    .map_err(|e| format!("sandbox.key: issue failed: {}", e))?;
                Ok(Value::Number(token_id as f64))
            }
            // v0.42.0: sandbox.check_call(token_id, "file.read") — authorize capability
            // Returns: Value::Bool(true) if authorized, false otherwise
            "check_call" => {
                if args.len() != 2 {
                    return Err(format!(
                        "sandbox.check_call: requires 2 args (token_id, capability), got {}",
                        args.len()
                    ));
                }
                let token_id = match &args[0] {
                    Value::Number(n) => *n as u64,
                    Value::Int(i) => *i as u64,
                    _ => {
                        return Err("sandbox.check_call: token_id must be a number".to_string());
                    }
                };
                let cap_str = match &args[1] {
                    Value::String(s) => s.clone(),
                    _ => {
                        return Err("sandbox.check_call: capability must be a string".to_string());
                    }
                };
                let cap = crate::sandbox::Capability::parse(&cap_str).ok_or_else(|| {
                    format!("sandbox.check_call: unknown capability '{}'", cap_str)
                })?;
                Ok(Value::Bool(
                    self.sandbox.capabilities.check(token_id, cap).is_ok(),
                ))
            }
            // v0.42.0: sandbox.revoke(token_id) — revoke capability token (bump generation)
            "revoke" => {
                if args.len() != 1 {
                    return Err(format!(
                        "sandbox.revoke: requires 1 arg (token_id), got {}",
                        args.len()
                    ));
                }
                let token_id = match &args[0] {
                    Value::Number(n) => *n as u64,
                    Value::Int(i) => *i as u64,
                    _ => {
                        return Err("sandbox.revoke: token_id must be a number".to_string());
                    }
                };
                self.sandbox
                    .capabilities
                    .revoke(token_id)
                    .map_err(|e| format!("sandbox.revoke: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.0: sandbox.token_count() — diagnostic
            "token_count" => Ok(Value::Number(self.sandbox.capabilities.token_count() as f64)),
            // v0.42.1: sandbox.audit_emit(actor, action, target?, payload?) — write audit event
            "audit_emit" => {
                if args.len() < 2 || args.len() > 4 {
                    return Err(format!(
                        "sandbox.audit_emit: requires 2-4 args (actor, action, target?, payload?), got {}",
                        args.len()
                    ));
                }
                let actor = match &args[0] {
                    Value::String(s) => s.clone(),
                    _ => return Err("sandbox.audit_emit: actor must be a string".to_string()),
                };
                let action = match &args[1] {
                    Value::String(s) => s.clone(),
                    _ => return Err("sandbox.audit_emit: action must be a string".to_string()),
                };
                let target = if args.len() >= 3 {
                    match &args[2] {
                        Value::String(s) if !s.is_empty() => Some(s.clone()),
                        Value::Nil | Value::String(_) => None,
                        _ => {
                            return Err(
                                "sandbox.audit_emit: target must be a string or nil".to_string()
                            );
                        }
                    }
                } else {
                    None
                };
                let payload = if args.len() >= 4 {
                    match &args[3] {
                        Value::String(s) if !s.is_empty() => Some(s.clone()),
                        Value::Nil | Value::String(_) => None,
                        _ => {
                            return Err(
                                "sandbox.audit_emit: payload must be a string or nil".to_string()
                            );
                        }
                    }
                } else {
                    None
                };
                let event = crate::audit::AuditEvent::new(actor, action, target, payload, None);
                self.audit_sink
                    .write(event)
                    .map_err(|e| format!("sandbox.audit_emit: write failed: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.1: sandbox.audit_flush() — flush audit sink to disk
            "audit_flush" => {
                self.audit_sink
                    .flush()
                    .map_err(|e| format!("sandbox.audit_flush: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.1: sandbox.audit_verify() — verify hash chain (returns true / error string)
            "audit_verify" => match self.audit_sink.verify_chain() {
                Ok(()) => Ok(Value::Bool(true)),
                Err(e) => Ok(Value::String(format!("{}", e))),
            },
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

#[cfg(test)]
mod tests_v042_capability {
    use super::*;
    use crate::value::Value;

    /// v0.42.0: sandbox.key + sandbox.check_call builtin 测试

    #[test]
    fn sandbox_key_returns_token_id_number() {
        let interp = Interpreter::new();
        let args = vec![
            Value::String("file.read".to_string()),
            Value::String("web.fetch".to_string()),
        ];
        let token_id = interp
            .call_sandbox_method("key", &args)
            .expect("sandbox.key should succeed");
        match token_id {
            Value::Number(n) => assert_eq!(n, 0.0, "first token_id should be 0"),
            other => panic!("expected Value::Number, got {:?}", other),
        }
        assert_eq!(interp.sandbox.capabilities.token_count(), 1);
    }

    #[test]
    fn sandbox_key_with_no_caps_returns_token() {
        // 空 args 也是合法: 创建一个空 capability 集合 (拒绝一切)
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[])
            .expect("sandbox.key with no args should succeed");
        assert!(matches!(token_id, Value::Number(_)));
        // 空 token 任何 cap 都应被拒绝
        let check = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call should not error");
        assert_eq!(check, Value::Bool(false));
    }

    #[test]
    fn sandbox_key_rejects_unknown_capability_string() {
        let interp = Interpreter::new();
        let args = vec![Value::String("not.a.real.cap".to_string())];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
        assert_eq!(interp.sandbox.capabilities.token_count(), 0);
    }

    #[test]
    fn sandbox_key_rejects_non_string_arg() {
        let interp = Interpreter::new();
        let args = vec![Value::Number(42.0)];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with non-string arg should error");
        assert!(err.contains("capability strings"), "got: {}", err);
    }

    #[test]
    fn sandbox_check_call_authorizes_granted_capability() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue token");

        let authorized = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(authorized, Value::Bool(true));

        let denied = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.write".to_string())],
            )
            .expect("check_call");
        assert_eq!(denied, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_token_returns_false() {
        let interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "check_call",
                &[
                    Value::Number(9999.0),
                    Value::String("file.read".to_string()),
                ],
            )
            .expect("check_call should not error, just return false");
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_capability_string_errors() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue");
        let err = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("not.a.cap".to_string())],
            )
            .expect_err("unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
    }

    #[test]
    fn sandbox_revoke_bumps_generation() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue");
        let token_id_num = match &token_id {
            Value::Number(n) => *n as u64,
            _ => panic!("expected Number"),
        };

        // revoke 前 check_call 返回 true
        let before = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(before, Value::Bool(true));

        // revoke
        let revoked = interp
            .call_sandbox_method("revoke", std::slice::from_ref(&token_id))
            .expect("revoke");
        assert_eq!(revoked, Value::Bool(true));

        // generation 确实 bump 了 (token 仍在 store, 但 generation=1)
        let token = interp
            .sandbox
            .capabilities
            .get(token_id_num)
            .expect("token should still exist (loongclaw-style: bump gen, not delete)");
        assert_eq!(token.generation, 1);

        // 注: call_sandbox_method 不验证 generation (那是 PolicyEngine trait 行为)
        // 但 store.check 也不验证 — generation 是给业务层用的语义信号
        let after = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(
            after,
            Value::Bool(true),
            "token still permits (loongclaw style)"
        );
    }

    #[test]
    fn sandbox_token_count_tracks_unique_tokens() {
        let interp = Interpreter::new();
        assert_eq!(interp.sandbox.capabilities.token_count(), 0);

        let _ = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method("key", &[Value::String("web.fetch".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method(
                "key",
                &[
                    Value::String("memory.read".to_string()),
                    Value::String("memory.write".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(interp.sandbox.capabilities.token_count(), 3);
    }

    #[test]
    fn sandbox_old_methods_still_work() {
        // v0.42.0 增补不应破坏 v0.33-0.41 的 sandbox.mode / check_builtin / check_path
        let interp = Interpreter::new();
        let mode = interp.call_sandbox_method("mode", &[]).expect("mode");
        assert!(matches!(mode, Value::String(_)));

        let cb = interp
            .call_sandbox_method("check_builtin", &[Value::String("print".to_string())])
            .expect("check_builtin");
        assert_eq!(cb, Value::Bool(true));
    }
}

#[cfg(test)]
mod tests_v0421_audit {
    use super::*;
    use crate::audit::{AuditSink, JsonlAuditSink};
    use crate::value::Value;
    use std::sync::Arc;

    /// v0.42.1: sandbox.audit_emit / audit_flush / audit_verify builtin tests
    fn temp_log_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_audit_builtin_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn audit_emit_writes_event_and_returns_true() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_basic.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.audit_sink = sink.clone();

        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("file.write".to_string()),
                    Value::String("/tmp/foo.txt".to_string()),
                    Value::String("{\"size\":42}".to_string()),
                ],
            )
            .expect("audit_emit");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_with_optional_args() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_minimal.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.audit_sink = sink.clone();

        // 仅 actor + action
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("agent".to_string()),
                    Value::String("chat.start".to_string()),
                ],
            )
            .expect("audit_emit minimal");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_validates_arg_types() {
        let interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::Number(42.0), Value::String("action".to_string())],
            )
            .expect_err("non-string actor should fail");
        assert!(err.contains("actor must be a string"), "got: {}", err);
    }

    #[test]
    fn audit_emit_validates_arg_count() {
        let interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::String("a".to_string())], // 只 1 个 arg
            )
            .expect_err("too few args should fail");
        assert!(err.contains("2-4 args"), "got: {}", err);
    }

    #[test]
    fn audit_flush_and_verify_chain_passes() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("verify.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.audit_sink = sink.clone();

        for i in 0..5 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("user".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit");
        }
        let flushed = interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");
        assert_eq!(flushed, Value::Bool(true));

        let verified = interp
            .call_sandbox_method("audit_verify", &[])
            .expect("verify");
        assert_eq!(verified, Value::Bool(true));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_verify_detects_tampering() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("tampered.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.audit_sink = sink.clone();

        for i in 0..3 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("a".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit");
        }
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");
        assert_eq!(
            interp.call_sandbox_method("audit_verify", &[]).unwrap(),
            Value::Bool(true)
        );

        // 篡改 line 1
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[1] = lines[1].replace("\"action\":\"op.1\"", "\"action\":\"TAMPERED\"");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let verified = interp.call_sandbox_method("audit_verify", &[]).unwrap();
        // 应返回 Value::String(error)
        match verified {
            Value::String(s) => assert!(
                s.contains("hash mismatch") || s.contains("HashMismatch"),
                "got: {}",
                s
            ),
            Value::Bool(true) => panic!("tamper should have been detected"),
            other => panic!("unexpected: {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn null_sink_default_audit_emit_returns_true() {
        // 默认 NullSink 应接受所有 audit_emit 调用
        let interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("op".to_string()),
                ],
            )
            .expect("audit_emit to null sink");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn audit_emit_writes_to_real_file_via_jsonl_sink() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("real_file.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.audit_sink = sink.clone();

        interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("sandbox.issue".to_string()),
                    Value::Nil,
                    Value::String("{\"cap\":\"file.read\"}".to_string()),
                ],
            )
            .expect("emit");
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush");

        // 验证文件存在且包含期望字段
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"action\":\"sandbox.issue\""));
        assert!(content.contains("\"actor\":\"user\""));
        assert!(content.contains("\"payload\":\"{\\\"cap\\\":\\\"file.read\\\"}\""));
        assert!(content.contains("\"hash\":"));

        let _ = std::fs::remove_file(&path);
    }
}

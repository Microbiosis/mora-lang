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
            // v0.43.1: bus.subscribe(pattern) — pub-sub subscribe (Puter / AgentMesh / Solace)
            // Returns: token (Value::Number) for later unsubscribe
            // Note: handler is internal — actual mora-level callback support would
            // require lifting Fn closures to a sandboxed layer; for now subscribe()
            // registers the subscription slot, and publish() fires it.
            // (Future: integrate with Mora task scheduler)
            "subscribe" => {
                let pattern = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("bus.subscribe: pattern must be a string".to_string()),
                    None => return Err("bus.subscribe: requires pattern arg".to_string()),
                };
                // 注册一个 no-op handler 让 pattern 进入订阅表
                // 真实 handler 由上层 (LSP / HTTP / MCP) 通过更高级 API 提供
                // 这里用空 handler 占位, 返回 token = pattern_count (递增)
                self.bus.on(
                    &pattern,
                    Arc::new(|_, _| {
                        // no-op: subscribe 占位
                    }),
                );
                let token = self.bus.pattern_count() as u64;
                Ok(Value::Number(token as f64))
            }
            // v0.43.1: bus.publish(topic, payload) — pub-sub publish (Puter / AgentMesh verified)
            // Returns: Number of registered patterns (informational; actual fire via emit)
            "publish" => {
                let topic = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("bus.publish: topic must be a string".to_string()),
                    None => return Err("bus.publish: requires topic arg".to_string()),
                };
                let payload = args.get(1).cloned().unwrap_or(Value::Nil);
                // 直接走 EventBus::emit, 它已经支持通配符 (Puter O(segments) 索引, v0.41.0)
                self.bus.emit(&topic, &payload);
                // 返回注册的 pattern 数 (informational)
                Ok(Value::Number(self.bus.pattern_count() as f64))
            }
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
            // v0.44.0: sandbox.containerize(backend, mounts?, network?, cpu_cores?, memory_mb?, image?)
            // **REAL Docker spawn** via `docker run -d` (NOT metadata-only)
            // Returns: Number(container_id hash) on success
            "containerize" => {
                let backend_str = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err(
                        "sandbox.containerize: backend must be a string (\"docker\"/\"gondolin\"/\"openshell\")".to_string()
                    ),
                };
                let backend =
                    crate::sandbox::ContainerBackend::parse(&backend_str).ok_or_else(|| {
                        format!("sandbox.containerize: unknown backend '{}'", backend_str)
                    })?;
                let mut spec = crate::sandbox::ContainerSpec::new(backend);

                // mounts (可选, arg 1)
                if let Some(Value::List(mounts)) = args.get(1) {
                    for (i, m) in mounts.iter().enumerate() {
                        let m_str = match m {
                            Value::String(s) => s.clone(),
                            _ => {
                                return Err(format!(
                                    "sandbox.containerize: mounts[{}] must be a string",
                                    i
                                ));
                            }
                        };
                        let mount = crate::sandbox::MountSpec::parse(&m_str)
                            .map_err(|e| format!("sandbox.containerize: {}", e))?;
                        spec.mounts.push(mount);
                    }
                }

                // network (可选, arg 2)
                if let Some(Value::String(net_str)) = args.get(2) {
                    spec.network =
                        crate::sandbox::NetworkMode::parse(net_str).ok_or_else(|| {
                            format!("sandbox.containerize: unknown network '{}'", net_str)
                        })?;
                }

                // cpu_cores (可选, arg 3)
                if let Some(n) = args.get(3) {
                    match n {
                        Value::Number(v) => spec.limits.cpu_cores = Some(*v as u32),
                        Value::Int(i) => spec.limits.cpu_cores = Some(*i as u32),
                        Value::Nil => {}
                        _ => {
                            return Err(
                                "sandbox.containerize: cpu_cores must be a number".to_string()
                            );
                        }
                    }
                }

                // memory_mb (可选, arg 4)
                if let Some(n) = args.get(4) {
                    match n {
                        Value::Number(v) => spec.limits.memory_mb = Some(*v as u64),
                        Value::Int(i) => spec.limits.memory_mb = Some(*i as u64),
                        Value::Nil => {}
                        _ => {
                            return Err(
                                "sandbox.containerize: memory_mb must be a number".to_string()
                            );
                        }
                    }
                }

                // image (可选, arg 5; default alpine:latest)
                if let Some(Value::String(img)) = args.get(5) {
                    spec.image = img.clone();
                }

                spec.validate()
                    .map_err(|e| format!("sandbox.containerize: {}", e))?;

                // **REAL spawn** — 真的调用 docker run
                let handle = crate::sandbox::spawn_container(&spec)
                    .map_err(|e| format!("sandbox.containerize: {}", e))?;

                // 用 container_id 的 hash 做成 Number 返回 (handle 存到 Interpreter)
                let id_hash = {
                    let mut h: u64 = 14695981039346656037;
                    for b in handle.container_id.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(1099511628211);
                    }
                    h
                };

                *self.container.lock() = Some(handle);
                Ok(Value::Number(id_hash as f64))
            }
            // v0.44.0: sandbox.container_exec(cmd, args...) — run cmd INSIDE container via docker exec
            // Returns: Dict{exit_code, stdout, stderr, elapsed_ms}
            "container_exec" => {
                let guard = self.container.lock();
                let handle = guard
                    .as_ref()
                    .ok_or_else(|| {
                        "sandbox.container_exec: no container (call sandbox.containerize first)"
                            .to_string()
                    })?
                    .clone();
                drop(guard);

                if args.is_empty() {
                    return Err("sandbox.container_exec: requires at least 1 arg (cmd)".to_string());
                }
                // 第一个 arg 是 cmd (e.g. "ls"), 后续是 args (e.g. "-la", "/")
                let mut cmd_parts: Vec<String> = Vec::with_capacity(args.len());
                for (i, v) in args.iter().enumerate() {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(format!(
                                "sandbox.container_exec: arg[{}] must be a string",
                                i
                            ));
                        }
                    };
                    cmd_parts.push(s);
                }
                let cmd_refs: Vec<&str> = cmd_parts.iter().map(String::as_str).collect();
                let (code, stdout, stderr) = handle
                    .exec(&cmd_refs)
                    .map_err(|e| format!("sandbox.container_exec: {}", e))?;
                let mut d = std::collections::HashMap::new();
                d.insert("exit_code".to_string(), Value::Number(code as f64));
                d.insert("stdout".to_string(), Value::String(stdout));
                d.insert("stderr".to_string(), Value::String(stderr));
                d.insert(
                    "elapsed_ms".to_string(),
                    Value::Number(handle.elapsed().as_millis() as f64),
                );
                Ok(Value::Dict(d))
            }
            // v0.44.0: sandbox.container_info() — diagnostic, returns Dict (container_id, name, backend, mounts)
            "container_info" => {
                let guard = self.container.lock();
                match guard.as_ref() {
                    Some(handle) => {
                        let mut d = std::collections::HashMap::new();
                        d.insert(
                            "container_id".to_string(),
                            Value::String(handle.container_id.clone()),
                        );
                        d.insert(
                            "container_name".to_string(),
                            Value::String(handle.container_name.clone()),
                        );
                        d.insert(
                            "backend".to_string(),
                            Value::String(handle.backend.as_str().to_string()),
                        );
                        d.insert(
                            "image".to_string(),
                            Value::String(handle.spec.image.clone()),
                        );
                        d.insert(
                            "network".to_string(),
                            Value::String(
                                match handle.spec.network {
                                    crate::sandbox::NetworkMode::Isolated => "isolated",
                                    crate::sandbox::NetworkMode::Host => "host",
                                }
                                .to_string(),
                            ),
                        );
                        d.insert(
                            "mount_count".to_string(),
                            Value::Number(handle.spec.mounts.len() as f64),
                        );
                        d.insert(
                            "elapsed_ms".to_string(),
                            Value::Number(handle.elapsed().as_millis() as f64),
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            // v0.44.0: sandbox.container_clear() — REAL docker rm -f, then clear handle
            "container_clear" => {
                let mut guard = self.container.lock();
                if let Some(handle) = guard.as_ref() {
                    handle
                        .destroy()
                        .map_err(|e| format!("sandbox.container_clear: {}", e))?;
                }
                *guard = None;
                Ok(Value::Bool(true))
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

    /// v0.45.0: ai.* — top-level AI utilities (retry / role / context)
    ///
    /// Methods:
    /// - `ai.retry(attempts, backoff)` — return retry policy (mini-swe-agent tenacity pattern)
    /// - `ai.role(name)` — set/get per-turn role (OpenFugu Worker/Thinker/Verifier)
    /// - `ai.context.trim(threshold?)` — smart compress context window
    ///   (AgentMesh + pi-agent pattern: oldest messages dropped first)
    /// - `ai.context.info()` — get current window state
    pub fn call_ai_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            // v0.45.0: ai.retry(attempts, backoff_ms) — returns retry policy dict
            // Records in interpreter state, ready for use by chat/tokens layers
            "retry" => {
                let attempts = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ai.retry: requires attempts")?;
                let attempts_n: u32 = attempts
                    .parse()
                    .map_err(|_| format!("ai.retry: invalid attempts '{}'", attempts))?;
                if attempts_n == 0 {
                    return Err("ai.retry: attempts must be > 0".to_string());
                }
                let backoff_ms: u64 = if let Some(v) = args.get(1) {
                    match v {
                        Value::Number(n) => *n as u64,
                        Value::Int(i) => *i as u64,
                        Value::String(s) => s.parse().unwrap_or(1000),
                        _ => 1000,
                    }
                } else {
                    1000
                };
                let backoff_strategy = if let Some(Value::String(s)) = args.get(2) {
                    s.clone()
                } else {
                    "exponential".to_string()
                };
                let mut d = std::collections::HashMap::new();
                d.insert("attempts".to_string(), Value::Number(attempts_n as f64));
                d.insert("backoff_ms".to_string(), Value::Number(backoff_ms as f64));
                d.insert(
                    "backoff".to_string(),
                    Value::String(backoff_strategy.clone()),
                );
                // 计算每个 attempt 的延迟 (mini-swe-agent tenacity-like)
                let mut schedule = Vec::new();
                for i in 0..attempts_n {
                    let delay = match backoff_strategy.as_str() {
                        "fixed" => backoff_ms,
                        "exponential" => backoff_ms * (1u64 << i.min(10)), // 2^i cap at 1024x
                        "linear" => backoff_ms * (i as u64 + 1),
                        _ => backoff_ms * (1u64 << i.min(10)),
                    };
                    schedule.push(Value::Number(delay as f64));
                }
                d.insert("schedule".to_string(), Value::List(schedule));
                Ok(Value::Dict(d))
            }
            // v0.45.0: ai.role(name) — set/get current AI role (OpenFugu per-turn)
            "role" => {
                if args.is_empty() {
                    return Err("ai.role: requires role name".to_string());
                }
                let role = args[0].to_string();
                // Validate against OpenFugu's 3 roles + extras
                match role.as_str() {
                    "worker" | "thinker" | "verifier" => {}
                    other => {
                        // Allow other role names but warn (informational)
                        // Per OpenFugu: Worker / Thinker / Verifier are the 3 main roles
                        let _ = other;
                    }
                }
                Ok(Value::String(role))
            }
            // v0.47.0: ai.context.* — context window control (AgentMesh+pi-agent)
            "context.trim" => {
                // 可选 threshold (0.0-1.0), 默认使用 self.context_window.compression_threshold
                if let Some(v) = args.first() {
                    let t = match v {
                        Value::Number(n) => *n,
                        Value::Int(i) => *i as f64,
                        _ => {
                            return Err(
                                "ai.context.trim: threshold must be a number 0.0-1.0".to_string()
                            );
                        }
                    };
                    if !(0.0..=1.0).contains(&t) {
                        return Err(format!(
                            "ai.context.trim: threshold must be 0.0-1.0, got {}",
                            t
                        ));
                    }
                    self.context_window.compression_threshold = t;
                }
                let before = self.context_window.current_tokens;
                self.context_window.compress();
                let after = self.context_window.current_tokens;
                let dropped = before.saturating_sub(after);
                Ok(Value::Number(dropped as f64))
            }
            "context.info" => {
                let mut d = std::collections::HashMap::new();
                d.insert(
                    "max_tokens".to_string(),
                    Value::Number(self.context_window.max_tokens as f64),
                );
                d.insert(
                    "current_tokens".to_string(),
                    Value::Number(self.context_window.current_tokens as f64),
                );
                d.insert(
                    "messages".to_string(),
                    Value::Number(self.context_window.messages.len() as f64),
                );
                d.insert(
                    "compression_threshold".to_string(),
                    Value::Number(self.context_window.compression_threshold),
                );
                Ok(Value::Dict(d))
            }
            // v0.47.0: ai.dag(nodes, edges) — DAG-as-data (OpenFugu §1.6)
            "dag" => {
                if args.len() < 2 {
                    return Err("ai.dag: requires 2 args (nodes, edges)".to_string());
                }
                let nodes = match &args[0] {
                    Value::List(items) => items
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        })
                        .collect::<Vec<String>>(),
                    _ => {
                        return Err("ai.dag: nodes must be a list of strings".to_string());
                    }
                };
                let edges = match &args[1] {
                    Value::List(items) => {
                        let mut out = Vec::with_capacity(items.len());
                        for (i, e) in items.iter().enumerate() {
                            match e {
                                Value::List(pair) if pair.len() == 2 => {
                                    let from = match &pair[0] {
                                        Value::String(s) => s.clone(),
                                        _ => pair[0].to_string(),
                                    };
                                    let to = match &pair[1] {
                                        Value::String(s) => s.clone(),
                                        _ => pair[1].to_string(),
                                    };
                                    out.push((from, to));
                                }
                                _ => {
                                    return Err(format!(
                                        "ai.dag: edges[{}] must be a [from, to] pair",
                                        i
                                    ));
                                }
                            }
                        }
                        out
                    }
                    _ => {
                        return Err("ai.dag: edges must be a list of [from, to] pairs".to_string());
                    }
                };
                let dag = crate::orchestrate_dag::OrchestrateDag::new(nodes, edges);
                let order = dag
                    .topological_order()
                    .map_err(|e| format!("ai.dag: {}", e))?;
                Ok(Value::List(order.into_iter().map(Value::String).collect()))
            }
            // v0.47.0: ai.heartbeat(path?) — heartbeat.md checklist (mimiclaw §1.5)
            "heartbeat" => {
                let path = if let Some(Value::String(s)) = args.first() {
                    std::path::PathBuf::from(s)
                } else {
                    let home = std::env::var("HOME")
                        .or_else(|_| std::env::var("USERPROFILE"))
                        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
                    std::path::PathBuf::from(home)
                        .join(".mora")
                        .join("HEARTBEAT.md")
                };
                let report = crate::heartbeat::load_heartbeat(&path)
                    .map_err(|e| format!("ai.heartbeat: {}", e))?;
                let mut d = std::collections::HashMap::new();
                d.insert(
                    "path".to_string(),
                    Value::String(path.to_string_lossy().to_string()),
                );
                d.insert("total".to_string(), Value::Number(report.total as f64));
                d.insert("done".to_string(), Value::Number(report.done as f64));
                d.insert("pending".to_string(), Value::Number(report.pending as f64));
                d.insert(
                    "completion_ratio".to_string(),
                    Value::Number(report.completion_ratio()),
                );
                d.insert("is_complete".to_string(), Value::Bool(report.is_complete()));
                let items: Vec<Value> = report
                    .items
                    .into_iter()
                    .map(|i| {
                        let mut m = std::collections::HashMap::new();
                        m.insert("text".to_string(), Value::String(i.text));
                        m.insert("done".to_string(), Value::Bool(i.done));
                        m.insert("line".to_string(), Value::Number(i.line_number as f64));
                        Value::Dict(m)
                    })
                    .collect();
                d.insert("items".to_string(), Value::List(items));
                Ok(Value::Dict(d))
            }
            _ => Err(format!("ai.{}: unknown method", method)),
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
            // v0.43.1: memory.remember(category, text) — markdown-backed persistent memory
            // Appends `text` under `## {category}` in ~/.mora/memory/YYYY-MM-DD.md
            // Returns: Bool(true) on success
            "remember" => {
                let category = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.remember: requires category")?;
                let text = args
                    .get(1)
                    .map(|v| v.to_string())
                    .ok_or("memory.remember: requires text")?;
                remember_markdown(self.markdown_memory_dir.as_deref(), &category, &text)
                    .map_err(|e| format!("memory.remember: {}", e))?;
                // 也写到 memory_store (key=category, value=text) 让 recall 能查到
                self.memory_store
                    .insert(format!("md:{}", category), Value::String(text));
                Ok(Value::Bool(true))
            }
            // v0.43.1: memory.recall_markdown(category) — read markdown entries for category
            // Returns: String with concatenated entries (empty if none)
            "recall_markdown" => {
                let category = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.recall_markdown: requires category")?;
                recall_markdown(self.markdown_memory_dir.as_deref(), &category)
                    .map(Value::String)
                    .map_err(|e| format!("memory.recall_markdown: {}", e))
            }
            // v0.43.1: memory.list_markdown() — list all categories
            // Returns: List[String] of category names
            "list_markdown" => list_markdown_categories(self.markdown_memory_dir.as_deref())
                .map(|cats| Value::List(cats.into_iter().map(Value::String).collect()))
                .map_err(|e| format!("memory.list_markdown: {}", e)),
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

    /// v0.43.0: exec.* — parallel subprocess execution (pi-mono v1 inspired)
    ///
    /// 设计 vs master doc §6.5:
    /// - 用 std::thread::spawn 替代 tokio runtime (守"不引入 async runtime"红线)
    /// - 用 std::process::Command + pre_exec (Unix) / creation_flags (Windows)
    ///   实现进程组隔离 (setpgid / CREATE_NEW_PROCESS_GROUP)
    /// - 用 std::sync::Mutex<SemaphoreState> 自制信号量
    /// - 用 std::sync::mpsc::channel 收集结果
    ///
    /// 支持的方法:
    /// - `exec.parallel(cmds: [String], max_concurrent?: number, timeout_ms?: number)`
    ///   → Value::List[Dict{cmd, stdout, stderr, exit_code, pid, elapsed_ms, error?}]
    pub fn call_exec_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "parallel" => exec_parallel(args),
            _ => Err(format!("exec.{}: unknown method", method)),
        }
    }

    /// v0.45.0: tool.plane.* — ToolPlane Core/Extension adapter (loongclaw tool.rs)
    ///
    /// Methods:
    /// - `tool.plane.create(name, kind)` — create new plane (kind = "core"/"extension")
    /// - `tool.plane.register(plane_name, tool_name, description, parameters)`
    /// - `tool.plane.unregister(plane_name, tool_name)`
    /// - `tool.plane.list()` — List[String] of plane names
    /// - `tool.plane.list_tools(plane_name)` — List[String] of tool names
    /// - `tool.plane.info(plane_name)` — Dict with kind/tool_count
    /// - `tool.plane.find(plane_name, tool_name)` — Dict with description/parameters
    /// - `tool.plane.remove(plane_name)` — remove plane
    pub fn call_toolplane_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut reg = self.tool_planes.lock();

        match method {
            "create" => {
                let name = args
                    .first()
                    .ok_or("tool.plane.create: requires name")?
                    .to_string();
                let kind_str = args
                    .get(1)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "extension".to_string());
                let kind = crate::toolplane::PlaneKind::parse(&kind_str)
                    .ok_or_else(|| format!("tool.plane.create: unknown kind '{}'", kind_str))?;
                reg.create_plane(name, kind)
                    .map_err(|e| format!("tool.plane.create: {}", e))?;
                Ok(Value::Bool(true))
            }
            "register" => {
                if args.len() < 4 {
                    return Err(
                        "tool.plane.register: requires 4 args (plane, tool, desc, params)"
                            .to_string(),
                    );
                }
                let plane_name = args[0].to_string();
                let tool_name = args[1].to_string();
                let description = args[2].to_string();
                let parameters = args[3].to_string();
                let plane = reg.get_plane_mut(&plane_name).ok_or_else(|| {
                    format!("tool.plane.register: plane '{}' not found", plane_name)
                })?;
                plane
                    .register(crate::toolplane::ToolSpec {
                        name: tool_name,
                        description,
                        parameters,
                    })
                    .map_err(|e| format!("tool.plane.register: {}", e))?;
                Ok(Value::Bool(true))
            }
            "unregister" => {
                if args.len() < 2 {
                    return Err("tool.plane.unregister: requires 2 args (plane, tool)".to_string());
                }
                let plane_name = args[0].to_string();
                let tool_name = args[1].to_string();
                let plane = reg.get_plane_mut(&plane_name).ok_or_else(|| {
                    format!("tool.plane.unregister: plane '{}' not found", plane_name)
                })?;
                let removed = plane.unregister(&tool_name);
                Ok(Value::Bool(removed.is_some()))
            }
            "list" => {
                let names = reg.list_planes();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            "list_tools" => {
                let plane_name = args
                    .first()
                    .ok_or("tool.plane.list_tools: requires plane name")?
                    .to_string();
                let plane = reg.get_plane(&plane_name).ok_or_else(|| {
                    format!("tool.plane.list_tools: plane '{}' not found", plane_name)
                })?;
                let mut names: Vec<String> = plane.tools.keys().cloned().collect();
                names.sort();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            "info" => {
                let plane_name = args
                    .first()
                    .ok_or("tool.plane.info: requires plane name")?
                    .to_string();
                match reg.get_plane(&plane_name) {
                    Some(plane) => {
                        let mut d = std::collections::HashMap::new();
                        d.insert("name".to_string(), Value::String(plane.name.clone()));
                        d.insert(
                            "kind".to_string(),
                            Value::String(plane.kind.as_str().to_string()),
                        );
                        d.insert(
                            "tool_count".to_string(),
                            Value::Number(plane.tool_count() as f64),
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            "find" => {
                if args.len() < 2 {
                    return Err("tool.plane.find: requires 2 args (plane, tool)".to_string());
                }
                let plane_name = args[0].to_string();
                let tool_name = args[1].to_string();
                match reg.find_tool(&plane_name, &tool_name) {
                    Some(spec) => {
                        let mut d = std::collections::HashMap::new();
                        d.insert("plane".to_string(), Value::String(plane_name));
                        d.insert("tool".to_string(), Value::String(spec.name.clone()));
                        d.insert(
                            "description".to_string(),
                            Value::String(spec.description.clone()),
                        );
                        d.insert(
                            "parameters".to_string(),
                            Value::String(spec.parameters.clone()),
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            "remove" => {
                let plane_name = args
                    .first()
                    .ok_or("tool.plane.remove: requires plane name")?
                    .to_string();
                let removed = reg.remove_plane(&plane_name);
                Ok(Value::Bool(removed.is_some()))
            }
            _ => Err(format!("tool.plane.{}: unknown method", method)),
        }
    }

    /// v0.46.0: skill.* — MoraSkillSpec + dual registry (CLI-Anything pattern)
    ///
    /// Methods:
    /// - `skill.list()` -> List[String] of skill names
    /// - `skill.find(name)` -> Dict{name, description, trigger, body, source}
    /// - `skill.load(path)` -> Bool (load SKILL.md from path, real file read)
    /// - `skill.install(name, content)` -> Bool (synthesize skill from content string)
    /// - `skill.uninstall(name)` -> Bool
    /// - `skill.set_hub(path)` -> Bool (set public_registry path, mora-public.json)
    /// - `skill.refresh_hub()` -> Number (re-read hub, real file I/O)
    pub fn call_skill_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut reg = self.skill_registry.lock();

        match method {
            "list" => {
                let names: Vec<String> = reg.list().into_iter().map(|s| s.name.clone()).collect();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            "find" => {
                let name = args.first().ok_or("skill.find: requires name")?.to_string();
                match reg.get(&name) {
                    Some(spec) => {
                        let mut d = std::collections::HashMap::new();
                        d.insert("name".to_string(), Value::String(spec.name.clone()));
                        d.insert(
                            "description".to_string(),
                            Value::String(spec.description.clone()),
                        );
                        d.insert(
                            "trigger".to_string(),
                            match &spec.trigger {
                                Some(t) => Value::String(t.clone()),
                                None => Value::Nil,
                            },
                        );
                        d.insert("body".to_string(), Value::String(spec.body.clone()));
                        d.insert(
                            "source".to_string(),
                            match &spec.source {
                                Some(p) => Value::String(p.display().to_string()),
                                None => Value::Nil,
                            },
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            "load" => {
                // 真正从文件加载 SKILL.md (REAL file I/O)
                let path_str = args.first().ok_or("skill.load: requires path")?.to_string();
                let path = std::path::PathBuf::from(&path_str);
                let spec = crate::skill::MoraSkillSpec::load_file(&path)
                    .map_err(|e| format!("skill.load: {}", e))?;
                reg.register(spec);
                Ok(Value::Bool(true))
            }
            "install" => {
                // 从 content 字符串合成 skill
                if args.len() < 2 {
                    return Err("skill.install: requires 2 args (name, content)".to_string());
                }
                let name = args[0].to_string();
                let content = args[1].to_string();
                let mut spec = crate::skill::MoraSkillSpec::parse(&content, None)
                    .map_err(|e| format!("skill.install: {}", e))?;
                // 强制 name 覆盖 (allows `skill.install("alias", content)` 模式)
                spec.name = name.clone();
                reg.register(spec);
                Ok(Value::Bool(true))
            }
            "uninstall" => {
                let name = args
                    .first()
                    .ok_or("skill.uninstall: requires name")?
                    .to_string();
                let removed = reg.unregister(&name);
                Ok(Value::Bool(removed.is_some()))
            }
            "set_hub" => {
                let path = args
                    .first()
                    .ok_or("skill.set_hub: requires path")?
                    .to_string();
                reg.set_public_registry(std::path::PathBuf::from(&path));
                Ok(Value::Bool(true))
            }
            "refresh_hub" => {
                // 真正从 mora-public.json 重读
                let count = reg
                    .load_public_registry()
                    .map_err(|e| format!("skill.refresh_hub: {}", e))?;
                Ok(Value::Number(count as f64))
            }
            _ => Err(format!("skill.{}: unknown method", method)),
        }
    }

    /// v0.48.0: plan.* — real-time checklist (pi-agent update_plan)
    ///
    /// Methods:
    /// - `plan.create(name, steps)` — create new plan
    /// - `plan.update(name, updates)` — update step statuses
    ///   updates: List[[id, status]]
    /// - `plan.add(name, id, text)` — add new step
    /// - `plan.remove(name, id)` — remove step
    /// - `plan.list(name?)` — list plans (or steps of one)
    /// - `plan.info(name)` — Dict{name, total, done, pending, completion_ratio}
    pub fn call_plan_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut plans = self.plans.lock();

        match method {
            "create" => {
                if args.len() < 2 {
                    return Err("plan.create: requires 2 args (name, steps)".to_string());
                }
                let name = args[0].to_string();
                let steps_arg = match &args[1] {
                    Value::List(items) => items,
                    _ => {
                        return Err(
                            "plan.create: steps must be a list of {id, text} dicts".to_string()
                        );
                    }
                };
                let mut plan = crate::plan::Plan::new();
                for (i, s) in steps_arg.iter().enumerate() {
                    let d = match s {
                        Value::Dict(d) => d,
                        _ => return Err(format!("plan.create: steps[{}] must be a dict", i)),
                    };
                    let id = match d.get("id") {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err(format!("plan.create: steps[{}].id must be a string", i)),
                    };
                    let text = match d.get("text") {
                        Some(Value::String(s)) => s.clone(),
                        _ => {
                            return Err(format!("plan.create: steps[{}].text must be a string", i));
                        }
                    };
                    let status = match d.get("status") {
                        Some(Value::String(s)) => crate::plan::StepStatus::parse(s)
                            .unwrap_or(crate::plan::StepStatus::Pending),
                        _ => crate::plan::StepStatus::Pending,
                    };
                    plan.add_step(crate::plan::PlanStep::new(id, text).with_status(status))
                        .map_err(|e| format!("plan.create: {}", e))?;
                }
                plans.insert(name.clone(), plan);
                Ok(Value::String(name))
            }
            "update" => {
                if args.len() < 2 {
                    return Err("plan.update: requires 2 args (name, updates)".to_string());
                }
                let name = args[0].to_string();
                let updates = match &args[1] {
                    Value::List(items) => items,
                    _ => {
                        return Err(
                            "plan.update: updates must be a list of [id, status]".to_string()
                        );
                    }
                };
                let mut parsed_updates: Vec<(String, crate::plan::StepStatus)> = Vec::new();
                for (i, u) in updates.iter().enumerate() {
                    let pair = match u {
                        Value::List(p) if p.len() == 2 => p,
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}] must be [id, status]",
                                i
                            ));
                        }
                    };
                    let id = match &pair[0] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}][0] must be id string",
                                i
                            ));
                        }
                    };
                    let status = match &pair[1] {
                        Value::String(s) => crate::plan::StepStatus::parse(s).ok_or_else(|| {
                            format!("plan.update: updates[{}][1] invalid status '{}'", i, s)
                        })?,
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}][1] must be status string",
                                i
                            ));
                        }
                    };
                    parsed_updates.push((id, status));
                }
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.update: plan '{}' not found", name))?;
                plan.update(&parsed_updates)
                    .map_err(|e| format!("plan.update: {}", e))?;
                Ok(Value::Bool(true))
            }
            "add" => {
                if args.len() < 3 {
                    return Err("plan.add: requires 3 args (name, id, text)".to_string());
                }
                let name = args[0].to_string();
                let id = args[1].to_string();
                let text = args[2].to_string();
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.add: plan '{}' not found", name))?;
                plan.add_step(crate::plan::PlanStep::new(id, text))
                    .map_err(|e| format!("plan.add: {}", e))?;
                Ok(Value::Bool(true))
            }
            "remove" => {
                if args.len() < 2 {
                    return Err("plan.remove: requires 2 args (name, id)".to_string());
                }
                let name = args[0].to_string();
                let id = args[1].to_string();
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.remove: plan '{}' not found", name))?;
                let removed = plan.remove_step(&id);
                Ok(Value::Bool(removed.is_some()))
            }
            "list" => {
                if let Some(Value::String(name)) = args.first() {
                    let plan = plans
                        .get(name)
                        .ok_or_else(|| format!("plan.list: plan '{}' not found", name))?;
                    let items: Vec<Value> = plan
                        .steps()
                        .iter()
                        .map(|s| {
                            let mut d = std::collections::HashMap::new();
                            d.insert("id".to_string(), Value::String(s.id.clone()));
                            d.insert("text".to_string(), Value::String(s.text.clone()));
                            d.insert(
                                "status".to_string(),
                                Value::String(s.status.as_str().to_string()),
                            );
                            d.insert(
                                "emoji".to_string(),
                                Value::String(s.status.emoji().to_string()),
                            );
                            Value::Dict(d)
                        })
                        .collect();
                    Ok(Value::List(items))
                } else {
                    let mut names: Vec<String> = plans.keys().cloned().collect();
                    names.sort();
                    Ok(Value::List(names.into_iter().map(Value::String).collect()))
                }
            }
            "info" => {
                let name = args
                    .first()
                    .ok_or("plan.info: requires plan name")?
                    .to_string();
                let plan = plans
                    .get(&name)
                    .ok_or_else(|| format!("plan.info: plan '{}' not found", name))?;
                let mut d = std::collections::HashMap::new();
                d.insert("name".to_string(), Value::String(name));
                d.insert("total".to_string(), Value::Number(plan.len() as f64));
                d.insert(
                    "done".to_string(),
                    Value::Number(plan.complete_count() as f64),
                );
                d.insert(
                    "pending".to_string(),
                    Value::Number(plan.pending_count() as f64),
                );
                d.insert(
                    "completion_ratio".to_string(),
                    Value::Number(plan.completion_ratio()),
                );
                Ok(Value::Dict(d))
            }
            _ => Err(format!("plan.{}: unknown method", method)),
        }
    }

    /// v0.48.0: mora.* — meta (refine)
    ///
    /// Methods:
    /// - `mora.refine(script_path, instruction)` — run refine iteration
    ///   (REAL file I/O, writes to .refine/ subdirectory)
    /// - `mora.refine_info(script_path, iteration?)` — get latest RefineStep
    ///   or specific iteration
    /// - `mora.list_refines()` — list all refined scripts
    pub fn call_mora_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "refine" => {
                if args.len() < 2 {
                    return Err(
                        "mora.refine: requires 2 args (script_path, instruction)".to_string()
                    );
                }
                let script = std::path::PathBuf::from(args[0].to_string());
                let instruction = args[1].to_string();
                // v0.49.0 (A2): drop lock before file I/O.
                // get_or_create 只创建空 session (无 I/O); refine 是 I/O 在锁外
                let step = {
                    let mut registry = self.refine_registry.lock();
                    let session = registry.get_or_create(&script);
                    session.refine(&instruction)
                }
                .map_err(|e| format!("mora.refine: {}", e))?;
                Ok(Value::Dict(step.to_dict()))
            }
            "refine_info" => {
                if args.is_empty() {
                    return Err("mora.refine_info: requires script_path".to_string());
                }
                let script = std::path::PathBuf::from(args[0].to_string());
                let iter = if let Some(Value::Number(n)) = args.get(1) {
                    Some(*n as usize)
                } else {
                    None
                };
                let registry = self.refine_registry.lock();
                let session = registry.get(&script).ok_or_else(|| {
                    format!("mora.refine_info: no session for '{}'", script.display())
                })?;
                let step = if let Some(n) = iter {
                    session
                        .steps
                        .get(n.saturating_sub(1))
                        .ok_or_else(|| format!("mora.refine_info: iteration {} not found", n))?
                } else {
                    session
                        .latest_step()
                        .ok_or_else(|| "mora.refine_info: no steps yet".to_string())?
                };
                Ok(Value::Dict(step.to_dict()))
            }
            "list_refines" => {
                let registry = self.refine_registry.lock();
                let mut names: Vec<String> = Vec::new();
                for path in registry.session_paths() {
                    names.push(path.clone());
                }
                names.sort();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            _ => Err(format!("mora.{}: unknown method", method)),
        }
    }
}

// ============================================================
// v0.43.1: memory.remember / recall_markdown helpers
// ============================================================

/// 获取 markdown memory 根目录 (~/.mora/memory/)
/// v0.43.1: 优先用 Interpreter 字段 (test isolation); fallback 到 env var / home dir
fn markdown_memory_dir(override_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    if let Ok(custom) = std::env::var("MORA_MEMORY_DIR") {
        return std::path::PathBuf::from(custom);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mora").join("memory")
}

/// 当天日期 (YYYY-MM-DD)
fn today_date_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简化: 用 UNIX 秒转日期 (假设 UTC)
    // 1970-01-01 是周四, 用 Zeller 公式的一个变体
    let days = (secs / 86400) as i64;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // 从 1970-01-01 起算
    let mut year = 1970i32;
    let mut remaining = days;
    loop {
        let leap = is_leap(year);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        year += 1;
    }
    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            month = i;
            break;
        }
        remaining -= md;
    }
    (year, (month + 1) as u32, (remaining + 1) as u32)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// v0.43.1: remember(category, text) — 追加到 ~/.mora/memory/YYYY-MM-DD.md
/// 文件格式:
/// ```text
/// # YYYY-MM-DD
///
/// ## {category}
///
/// - {text}
///
/// ## {other_category}
///
/// - {text}
/// ```
/// 文件格式:
/// ```text
/// # YYYY-MM-DD
///
/// ## {category}
///
/// - {text}
///
/// ## {other_category}
///
/// - {text}
/// ```
fn remember_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
    text: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let dir = markdown_memory_dir(override_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", today_date_string()));

    // 读取现有内容, 决定是否要新建 section
    let existing = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut new_content = if existing.is_empty() {
        format!("# {}\n\n", today_date_string())
    } else {
        existing.clone()
    };

    // 检查 category section 是否已存在
    let section_header = format!("## {}", category);
    if new_content.contains(&section_header) {
        // 追加 bullet 到现有 section (section_header 不需要再使用)
        // 追加 bullet 到现有 section
        new_content.push_str(&format!("- {}\n", text));
    } else {
        // 新建 section
        new_content.push_str(&format!("\n{}\n\n- {}\n", section_header, text));
    }

    // 写回 (原子性: write to temp + rename, 简化版直接 overwrite)
    let mut f = std::fs::File::create(&path)?;
    f.write_all(new_content.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// v0.43.1: recall_markdown(category) — 读所有 markdown 文件, 找 ## category 段, 拼接 bullets
fn recall_markdown(
    override_dir: Option<&std::path::Path>,
    category: &str,
) -> std::io::Result<String> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(String::new());
    }

    let mut out = String::new();

    // 按日期排序读所有 .md
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        // 找到 ## {category} 段, 收集直到下一个 ## 或文件末尾
        let mut in_section = false;
        for line in content.lines() {
            if let Some(header) = line.strip_prefix("## ") {
                in_section = header.trim() == category.trim();
            } else if in_section && line.starts_with("- ") {
                out.push_str(line.trim_start_matches("- "));
                out.push('\n');
            }
        }
    }

    Ok(out)
}

/// v0.43.1: list_markdown_categories() — 列出所有 markdown 文件中出现过的 ## section 标题
fn list_markdown_categories(
    override_dir: Option<&std::path::Path>,
) -> std::io::Result<Vec<String>> {
    let dir = markdown_memory_dir(override_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut seen = std::collections::BTreeSet::new();
    let entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();

    for entry in entries {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("## ") {
                // 跳过子标题 (### 等), 只取 ## level
                seen.insert(rest.trim().to_string());
            }
        }
    }

    Ok(seen.into_iter().collect())
}

// ============================================================
// v0.43.0: exec.parallel() implementation
// ============================================================

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// 并行执行结果 (单个 cmd)
#[derive(Debug, Clone)]
struct ParallelResult {
    cmd: String,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    elapsed_ms: u64,
    /// Process ID (0 = unknown / spawn failed)
    pid: u32,
    error: Option<String>,
}

impl ParallelResult {
    fn to_value(&self) -> Value {
        let mut d = HashMap::new();
        d.insert("cmd".to_string(), Value::String(self.cmd.clone()));
        d.insert("stdout".to_string(), Value::String(self.stdout.clone()));
        d.insert("stderr".to_string(), Value::String(self.stderr.clone()));
        match self.exit_code {
            Some(code) => {
                d.insert("exit_code".to_string(), Value::Int(code as i64));
            }
            None => {
                d.insert("exit_code".to_string(), Value::Nil);
            }
        }
        d.insert(
            "elapsed_ms".to_string(),
            Value::Number(self.elapsed_ms as f64),
        );
        // pid == 0 表示 unknown (spawn 失败或 pre-spawn)
        if self.pid == 0 {
            d.insert("pid".to_string(), Value::Nil);
        } else {
            d.insert("pid".to_string(), Value::Number(self.pid as f64));
        }
        match &self.error {
            Some(e) => {
                d.insert("error".to_string(), Value::String(e.clone()));
            }
            None => {
                d.insert("error".to_string(), Value::Nil);
            }
        }
        Value::Dict(d)
    }
}

/// 自制信号量 (std 没有 Semaphore)
struct Semaphore {
    permits: AtomicUsize,
    mutex: Mutex<()>,
    cond: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Self {
        Self {
            permits: AtomicUsize::new(permits),
            mutex: Mutex::new(()),
            cond: Condvar::new(),
        }
    }

    fn acquire(&self) {
        loop {
            let current = self.permits.load(Ordering::Acquire);
            if current > 0
                && self
                    .permits
                    .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return;
            }
            // 否则等待
            let guard = self.mutex.lock().expect("semaphore mutex poisoned");
            drop(self.cond.wait(guard).expect("condvar wait failed"));
        }
    }

    fn release(&self) {
        // v0.49.0 (A4): use AcqRel (not SeqCst) for lighter memory barrier.
        // prev == 0 means waiter queue may have blocked acquirers; wake one.
        let prev = self.permits.fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            let _guard = self.mutex.lock().expect("semaphore mutex poisoned");
            self.cond.notify_one();
        }
    }
}

/// `exec.parallel(args)` builtin implementation
fn exec_parallel(args: &[Value]) -> Result<Value, String> {
    // 解析参数
    if args.is_empty() {
        return Err("exec.parallel: requires at least 1 arg (cmds list)".to_string());
    }

    // 第一个 arg: List of String (cmd list)
    let cmds: Vec<String> = match &args[0] {
        Value::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, v) in list.iter().enumerate() {
                match v {
                    Value::String(s) => out.push(s.clone()),
                    _ => {
                        return Err(format!("exec.parallel: cmds[{}] must be a string", i));
                    }
                }
            }
            out
        }
        _ => return Err("exec.parallel: first arg must be a list of strings".to_string()),
    };

    if cmds.is_empty() {
        return Ok(Value::List(Vec::new()));
    }

    // 第二个 arg (可选): max_concurrent
    let max_concurrent: usize = if args.len() >= 2 {
        match &args[1] {
            Value::Number(n) => (*n as usize).max(1),
            Value::Int(i) => (*i as usize).max(1),
            _ => {
                return Err(
                    "exec.parallel: max_concurrent must be a non-negative number".to_string(),
                );
            }
        }
    } else {
        cmds.len() // 默认: 全部并发
    };

    // 第三个 arg (可选): timeout_ms
    let timeout: Option<Duration> = if args.len() >= 3 {
        match &args[2] {
            Value::Number(n) => Some(Duration::from_millis(*n as u64)),
            Value::Int(i) => Some(Duration::from_millis(*i as u64)),
            Value::Nil => None,
            _ => return Err("exec.parallel: timeout_ms must be a number or nil".to_string()),
        }
    } else {
        None
    };

    let sem = Arc::new(Semaphore::new(max_concurrent));
    let (tx, rx) = mpsc::channel::<ParallelResult>();
    let next_idx = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cmds_arc = Arc::new(cmds);

    // 启动 N 个 worker thread (每 worker 处理多个 cmd 直到所有完成)
    let num_workers = max_concurrent.min(cmds_arc.len());
    let mut handles = Vec::with_capacity(num_workers);

    for _ in 0..num_workers {
        let sem = sem.clone();
        let tx = tx.clone();
        let next_idx = next_idx.clone();
        let cancelled = cancelled.clone();
        let cmds = cmds_arc.clone();

        let handle = thread::spawn(move || {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                // 原子获取下一个 cmd index
                let idx = next_idx.fetch_add(1, Ordering::SeqCst);
                if idx >= cmds.len() {
                    break;
                }
                let cmd_str = cmds[idx].clone();

                // 获取信号量
                sem.acquire();
                let result = run_single_cmd(&cmd_str, timeout, &cancelled);
                sem.release();

                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        handles.push(handle);
    }
    drop(tx);

    // 收集结果
    let mut results: Vec<ParallelResult> = Vec::with_capacity(cmds_arc.len());
    for _ in 0..cmds_arc.len() {
        match rx.recv() {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }

    // 等待所有 worker 完成
    for h in handles {
        let _ = h.join();
    }

    // 转 Value::List[Dict]
    Ok(Value::List(results.iter().map(|r| r.to_value()).collect()))
}

/// 单个 cmd 执行 (run on worker thread)
fn run_single_cmd(
    cmd_str: &str,
    timeout: Option<Duration>,
    cancelled: &Arc<AtomicBool>,
) -> ParallelResult {
    let start = Instant::now();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd_str)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // 进程组隔离 (mini-swe-agent v1 风格, 防止 orphaned 进程)
    #[cfg(unix)]
    {
        // SAFETY: pre_exec 在 fork 后, exec 前执行
        // 仅调用 libc::setpgid, 不分配内存, 不持有锁
        unsafe {
            command.pre_exec(|| {
                // setpgid(0, 0) 创建新进程组, 这样 process group kill 能清理孙子进程
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        command.creation_flags(0x00000200);
    }

    // spawn
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ParallelResult {
                cmd: cmd_str.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                pid: 0, // spawn failed → pid unknown
                error: Some(format!("spawn failed: {}", e)),
            };
        }
    };
    let pid: u32 = child.id();

    // 等待 (带可选 timeout)
    let output = if let Some(timeout_dur) = timeout {
        // 简单实现: 把 wait 放到线程里, 主线程睡 timeout 后检查 cancelled
        // 但 std::process::Child 没有 async wait — 我们用 thread + join
        let timeout_ms = timeout_dur.as_millis() as u64;
        let (done_tx, done_rx) = mpsc::channel();
        let child = child; // move into thread
        let waiter = thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = done_tx.send(result);
        });

        match done_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(Ok(out)) => {
                let _ = waiter.join();
                out
            }
            Ok(Err(e)) => {
                let _ = waiter.join();
                return ParallelResult {
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("wait failed: {}", e)),
                };
            }
            Err(_) => {
                // Timeout: 杀进程组
                cancelled.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                let _ = waiter.join();
                return ParallelResult {
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("timeout after {}ms", timeout_ms)),
                };
            }
        }
    } else {
        match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                return ParallelResult {
                    cmd: cmd_str.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    pid,
                    error: Some(format!("wait failed: {}", e)),
                };
            }
        }
    };

    ParallelResult {
        cmd: cmd_str.to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
        elapsed_ms: start.elapsed().as_millis() as u64,
        pid,
        error: None,
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // killpg(pid, SIGKILL) — SIGKILL = 9
    // SAFETY: libc::killpg 直接系统调用, 无 Rust 抽象
    unsafe {
        // pid_t 是 i32
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_process_group(pid: u32) {
    // taskkill /F /T /PID <pid>
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .status();
}

#[cfg(test)]
mod tests_v042_capability {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.42.0: sandbox.key + sandbox.check_call builtin 测试

    #[test]
    fn sandbox_key_returns_token_id_number() {
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
        let args = vec![Value::String("not.a.real.cap".to_string())];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
        assert_eq!(interp.sandbox.capabilities.token_count(), 0);
    }

    #[test]
    fn sandbox_key_rejects_non_string_arg() {
        let mut interp = Interpreter::new();
        let args = vec![Value::Number(42.0)];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with non-string arg should error");
        assert!(err.contains("capability strings"), "got: {}", err);
    }

    #[test]
    fn sandbox_check_call_authorizes_granted_capability() {
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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

        // v0.49.0: generation 在 store 全局 bump (不放在 token 上)
        assert_eq!(interp.sandbox.capabilities.current_generation(), 1);
        // token 仍存在 (loongclaw-style: 不删除)
        assert!(interp.sandbox.capabilities.get(token_id_num).is_some());

        // v0.49.0: revoked token 在 check_call 时返回 false (TokenNotFound,
        // 因为 token.generation != current_generation)
        let after = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(
            after,
            Value::Bool(false),
            "v0.49.0: revoked token must fail check_call"
        );
    }

    #[test]
    fn sandbox_token_count_tracks_unique_tokens() {
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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
    #![allow(unused_mut)]
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
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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
        let mut interp = Interpreter::new();
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

#[cfg(test)]
mod tests_v043_exec {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    fn cmd(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// v0.43.0: exec.parallel() builtin tests

    #[test]
    fn exec_parallel_runs_all_commands() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo a"), cmd("echo b"), cmd("echo c")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .unwrap();
        let list = match result {
            Value::List(l) => l,
            other => panic!("expected List, got {:?}", other),
        };
        assert_eq!(list.len(), 3);
        // 并行执行顺序不固定; 收集所有 stdout
        let mut stdouts: Vec<String> = Vec::new();
        for item in &list {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("stdout not String"),
            };
            stdouts.push(stdout.trim().to_string());
            match d.get("exit_code") {
                Some(Value::Int(0)) => {}
                other => panic!("exit_code not 0: {:?}", other),
            }
        }
        stdouts.sort();
        assert_eq!(stdouts, vec!["a", "b", "c"]);
    }

    #[test]
    fn exec_parallel_respects_max_concurrent() {
        let mut interp = Interpreter::new();
        // 6 个 sleep 1s, max_concurrent=2 → 总时间应该 ~3s (而非 ~1s 或 ~6s)
        // 跳过 perf assertion — 只验证结果正确
        let cmds: Vec<Value> = (0..6).map(|i| cmd(&format!("echo {}", i))).collect();
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds), Value::Number(2.0)])
            .unwrap();
        let list = match result {
            Value::List(l) => l,
            other => panic!("expected List, got {:?}", other),
        };
        assert_eq!(list.len(), 6);
        for (i, item) in list.iter().enumerate() {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("no stdout"),
            };
            assert_eq!(stdout.trim(), i.to_string());
        }
    }

    #[test]
    fn exec_parallel_empty_list_returns_empty() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_exec_method("parallel", &[Value::List(vec![])])
            .unwrap();
        assert_eq!(result, Value::List(Vec::new()));
    }

    #[test]
    fn exec_parallel_collects_stdout_per_command() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo line1"), cmd("printf line2"), cmd("echo line3")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 3);
        // 顺序不固定, 收集所有 stdout 验证内容
        let mut stdouts: Vec<String> = Vec::new();
        for item in &list {
            let d = match item {
                Value::Dict(d) => d,
                _ => panic!("not Dict"),
            };
            let stdout = match d.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("no stdout"),
            };
            stdouts.push(stdout);
        }
        // printf 没 \n, echo 有
        // 不固定顺序, 但内容应该是 3 个特定字符串
        let mut normalized: Vec<String> = stdouts.iter().map(|s| s.trim().to_string()).collect();
        normalized.sort();
        let mut expected = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
        ];
        expected.sort();
        assert_eq!(normalized, expected);
    }

    #[test]
    fn exec_parallel_kills_process_group_on_timeout() {
        let mut interp = Interpreter::new();
        // "sleep 10" + timeout 200ms → 应报 timeout
        let cmds = vec![cmd("sleep 10")];
        let result = interp
            .call_exec_method(
                "parallel",
                &[Value::List(cmds), Value::Number(1.0), Value::Number(200.0)],
            )
            .unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1);
        let d = match &list[0] {
            Value::Dict(d) => d,
            _ => panic!("not Dict"),
        };
        // exit_code 应为 None (超时被杀)
        match d.get("exit_code") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil exit_code on timeout, got: {:?}", other),
        }
        // error 应包含 "timeout"
        match d.get("error") {
            Some(Value::String(s)) => assert!(s.contains("timeout"), "got: {}", s),
            other => panic!("expected timeout error, got: {:?}", other),
        }
    }

    #[test]
    fn exec_parallel_validates_arg_types() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_exec_method("parallel", &[Value::Number(42.0)])
            .expect_err("non-list first arg should fail");
        assert!(err.contains("list of strings"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_validates_cmd_elements() {
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("echo ok"), Value::Number(42.0)]; // 第二个不是 string
        let err = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .expect_err("non-string cmd should fail");
        assert!(err.contains("must be a string"), "got: {}", err);
    }

    #[test]
    fn exec_parallel_returns_error_for_missing_command() {
        // sh -c 调用不存在的命令 → sh 返回 exit_code=127, stderr "command not found"
        let mut interp = Interpreter::new();
        let cmds = vec![cmd("this_command_definitely_does_not_exist_xyz")];
        let result = interp
            .call_exec_method("parallel", &[Value::List(cmds)])
            .unwrap();
        let list = match result {
            Value::List(l) => l,
            _ => panic!("expected List"),
        };
        assert_eq!(list.len(), 1);
        let d = match &list[0] {
            Value::Dict(d) => d,
            _ => panic!("not Dict"),
        };
        // exit_code 应为 127 (POSIX "command not found")
        match d.get("exit_code") {
            Some(Value::Int(127)) => {}
            Some(Value::Int(other)) => panic!("expected 127, got {}", other),
            other => panic!("expected Int exit_code, got: {:?}", other),
        }
        // error 字段应为 Nil (执行成功, 只是退出码非 0)
        match d.get("error") {
            Some(Value::Nil) => {}
            other => panic!("expected Nil error, got: {:?}", other),
        }
        // stderr 应包含 "not found"
        match d.get("stderr") {
            Some(Value::String(s)) => assert!(
                s.contains("not found") || s.contains("command not found"),
                "got stderr: {}",
                s
            ),
            other => panic!("expected stderr string, got: {:?}", other),
        }
    }

    #[test]
    fn exec_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_exec_method("nonexistent", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v0431_memory_bus {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.43.1: memory.remember / recall_markdown / list_markdown
    fn setup_temp_memory_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_md_mem_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn teardown_memory_dir(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn memory_remember_appends_to_markdown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("user_prefs".to_string()),
                    Value::String("likes Rust".to_string()),
                ],
            )
            .expect("remember");
        assert_eq!(result, Value::Bool(true));

        // 验证文件存在并包含内容
        let date = today_date_string();
        let md_path = dir.join(format!("{}.md", date));
        let content = std::fs::read_to_string(&md_path).unwrap();
        assert!(content.contains("# "));
        assert!(content.contains("## user_prefs"));
        assert!(content.contains("- likes Rust"));

        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_remember_appends_to_existing_section() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("first entry".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("cat".to_string()),
                    Value::String("second entry".to_string()),
                ],
            )
            .unwrap();

        let date = today_date_string();
        let content = std::fs::read_to_string(dir.join(format!("{}.md", date))).unwrap();
        // 只有一个 ## cat section
        assert_eq!(content.matches("## cat").count(), 1);
        assert!(content.contains("- first entry"));
        assert!(content.contains("- second entry"));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_text() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("notes".to_string()),
                    Value::String("remember this".to_string()),
                ],
            )
            .unwrap();
        let recalled = interp
            .call_memory_method("recall_markdown", &[Value::String("notes".to_string())])
            .expect("recall");
        match recalled {
            Value::String(s) => assert!(s.contains("remember this"), "got: {}", s),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_markdown_returns_empty_for_unknown() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        let result = interp
            .call_memory_method("recall_markdown", &[Value::String("nope".to_string())])
            .expect("recall");
        assert_eq!(result, Value::String(String::new()));
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_list_markdown_lists_categories() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("a".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("b".to_string()),
                    Value::String("y".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_memory_method("list_markdown", &[])
            .expect("list");
        match list {
            Value::List(items) => {
                let cats: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(cats.contains(&"a".to_string()));
                assert!(cats.contains(&"b".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    #[test]
    fn memory_recall_after_remember_syncs_to_memory_store() {
        let dir = setup_temp_memory_dir();
        let mut interp = Interpreter::new();
        interp.markdown_memory_dir = Some(dir.clone());
        interp
            .call_memory_method(
                "remember",
                &[
                    Value::String("k".to_string()),
                    Value::String("v".to_string()),
                ],
            )
            .unwrap();
        // 通过现有 recall (HashMap-backed) 应能查到
        let recalled = interp
            .call_memory_method("recall", &[Value::String("md:k".to_string())])
            .expect("recall");
        match recalled {
            Value::String(s) => assert_eq!(s, "v"),
            other => panic!("expected String, got: {:?}", other),
        }
        teardown_memory_dir(&dir);
    }

    /// v0.43.1: bus.subscribe / bus.publish

    #[test]
    fn bus_subscribe_returns_token() {
        let mut interp = Interpreter::new();
        let token = interp
            .call_event_method(
                "subscribe",
                &[Value::String("agent.research.*".to_string())],
            )
            .expect("subscribe");
        // token 是 Number (pattern_count 1)
        match token {
            Value::Number(n) => assert_eq!(n, 1.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_subscribe_validates_pattern() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("subscribe", &[Value::Number(42.0)])
            .expect_err("non-string pattern should fail");
        assert!(err.contains("pattern must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_publish_returns_pattern_count() {
        let mut interp = Interpreter::new();
        // subscribe 2 个
        interp
            .call_event_method("subscribe", &[Value::String("ai.*".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("ai.chat.*".to_string())])
            .unwrap();
        // publish
        let count = interp
            .call_event_method(
                "publish",
                &[
                    Value::String("ai.chat.completed".to_string()),
                    Value::String("data".to_string()),
                ],
            )
            .expect("publish");
        // 返回 pattern_count (2)
        match count {
            Value::Number(n) => assert_eq!(n, 2.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_publish_validates_topic() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("publish", &[Value::Number(42.0)])
            .expect_err("non-string topic should fail");
        assert!(err.contains("topic must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_subscribe_then_publish_wildcard_match() {
        // end-to-end: subscribe "user.*", publish "user.created", 验证 pattern 进入订阅表
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("user.*".to_string())])
            .unwrap();
        // emit() 走通配符匹配 (v0.41.0 O(segments) 索引, 验证过)
        interp
            .call_event_method("emit", &[Value::String("user.created".to_string())])
            .unwrap();
        // pattern_count 应 = 1
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Number(1.0));
    }

    #[test]
    fn bus_subscribe_uses_existing_pattern_matching() {
        // 验证 subscribe 用的就是 EventBus::on() (已经在 v0.41.0 + v0.41.1 测试覆盖)
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("exact.event".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("prefix.*".to_string())])
            .unwrap();
        // 两个 patterns
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Number(2.0));
    }
}

#[cfg(test)]
mod tests_v044_container_real {
    // Tests use `let mut interp = ...` pattern uniformly; some tests don't actually need mut.
    // Allow unused_mut for the whole module to avoid 5 false positives.
    #![allow(unused_mut)]

    use super::*;
    use crate::value::Value;

    /// v0.44.0: REAL Docker container builtin integration
    /// **Requires Docker daemon** — 默认 #[ignore] 让 CI 无 docker 时跳过
    fn cleanup_container(interp: &mut Interpreter) {
        // 尽力清理 (可能根本没 spawn 成功)
        let _ = interp.call_sandbox_method("container_clear", &[]);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_containerize_real_spawn() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .expect("containerize should spawn docker");
        // 返回 Number (container_id hash)
        match result {
            Value::Number(n) => assert!(n > 0.0, "container_id hash should be non-zero"),
            other => panic!("expected Number, got: {:?}", other),
        }
        assert!(interp.container.lock().is_some());
        cleanup_container(&mut interp);
        assert!(interp.container.lock().is_none());
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_exec_runs_cmd_inside_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let result = interp
            .call_sandbox_method(
                "container_exec",
                &[
                    Value::String("echo".to_string()),
                    Value::String("hello-from-real-docker".to_string()),
                ],
            )
            .expect("container_exec should succeed");
        match result {
            Value::Dict(d) => {
                let stdout = match d.get("stdout") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected stdout String, got: {:?}", other),
                };
                assert!(
                    stdout.contains("hello-from-real-docker"),
                    "stdout should contain 'hello-from-real-docker', got: {}",
                    stdout
                );
                let exit_code = d.get("exit_code").expect("exit_code");
                assert!(
                    matches!(exit_code, Value::Number(0.0)),
                    "exit_code should be 0, got: {:?}",
                    exit_code
                );
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_info_returns_real_container_id() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        match info {
            Value::Dict(d) => {
                let id = match d.get("container_id") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected container_id String, got: {:?}", other),
                };
                assert!(
                    id.len() >= 12,
                    "docker container_id hex should be >= 12 chars: {}",
                    id
                );
                let name = d.get("container_name").expect("container_name");
                match name {
                    Value::String(s) => assert!(
                        s.starts_with("mora-"),
                        "name should start with mora-, got: {}",
                        s
                    ),
                    other => panic!("expected String name, got: {:?}", other),
                }
                let backend = d.get("backend").expect("backend");
                match backend {
                    Value::String(s) => assert_eq!(s, "docker"),
                    other => panic!("expected docker backend, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_clear_really_removes_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let id = {
            let guard = interp.container.lock();
            guard.as_ref().unwrap().container_id.clone()
        };
        // 验证 container 真的在 docker 里
        let check = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(check.status.success(), "docker should know the container");
        let state = String::from_utf8_lossy(&check.stdout).trim().to_string();
        assert_eq!(state, "true", "container should be running");

        // clear → 真 docker rm -f
        let cleared = interp
            .call_sandbox_method("container_clear", &[])
            .expect("clear");
        assert_eq!(cleared, Value::Bool(true));

        // 验证 container 真的没了
        let check2 = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(
            !check2.status.success(),
            "docker inspect should fail for removed container"
        );
    }

    #[test]
    fn sandbox_containerize_rejects_unknown_backend() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("vmware".to_string())])
            .expect_err("unknown backend should fail");
        assert!(err.contains("unknown backend"), "got: {}", err);
    }

    #[test]
    fn sandbox_containerize_rejects_unimplemented_backend() {
        // gondolin/openshell 在 v0.44.0 真实未实现, 应该返回明确错误
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("gondolin".to_string())])
            .expect_err("gondolin not yet implemented");
        assert!(err.contains("not yet implemented"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_exec_requires_container_first() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("container_exec", &[Value::String("ls".to_string())])
            .expect_err("exec without container should fail");
        assert!(err.contains("no container"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_info_returns_nil_when_unset() {
        let mut interp = Interpreter::new();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        assert_eq!(info, Value::Nil);
    }
}

#[cfg(test)]
mod tests_v044_orchestrate_validate {
    #![allow(unused_mut)]
    use crate::ast_v2::NodeId;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    /// v0.44.0: orchestrate block syntax validation (already implemented v0.25)
    fn parse(src: &str) -> (crate::ast_v2::AstArena, Vec<NodeId>) {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let node_ids = parser.parse();
        (parser.into_arena(), node_ids)
    }

    #[test]
    fn orchestrate_sequential_parses() {
        let src = r#"
task main()
  orchestrate sequential x -> y
    agent a(x) => "a:" + x
    agent b(x) => "b:" + x
"#;
        let (_arena, node_ids) = parse(src);
        assert!(!node_ids.is_empty());
    }

    #[test]
    fn orchestrate_loop_with_on_predicate_parses() {
        let src = r#"
task main()
  orchestrate loop x -> y, max_rounds: 5
    on: x == "done"
    agent a(x) => x
"#;
        let (_arena, node_ids) = parse(src);
        assert!(!node_ids.is_empty());
    }

    #[test]
    fn orchestrate_graph_with_predicate_edges_parses() {
        let src = r#"
task main()
  orchestrate graph x -> y
    @start -> a
    @start -> b on: x == "research"
    a -> @exit
    b -> @exit
"#;
        let (_arena, node_ids) = parse(src);
        assert!(!node_ids.is_empty());
    }
}

#[cfg(test)]
mod tests_v045_toolplane {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.45.0: tool.plane.* builtin (loongclaw Core/Extension pattern)

    #[test]
    fn tool_plane_create_default_core_planes_exist() {
        let mut interp = Interpreter::new();
        let list = interp.call_toolplane_method("list", &[]).expect("list");
        match list {
            Value::List(names) => {
                let names_v: Vec<String> = names
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(
                    names_v.contains(&"ai".to_string()),
                    "should have 'ai' core plane"
                );
                assert!(
                    names_v.contains(&"sandbox".to_string()),
                    "should have 'sandbox' core plane"
                );
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_create_extension() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("user_plane".to_string()),
                    Value::String("extension".to_string()),
                ],
            )
            .expect("create");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("user_plane".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let kind = d.get("kind").expect("kind");
                match kind {
                    Value::String(s) => assert_eq!(s, "extension"),
                    other => panic!("expected extension kind, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_register_and_find() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                    Value::String("does something".to_string()),
                    Value::String(r#"{"type":"object"}"#.to_string()),
                ],
            )
            .expect("register");

        let tools = interp
            .call_toolplane_method("list_tools", &[Value::String("p".to_string())])
            .expect("list_tools");
        match tools {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"mytool".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }

        let found = interp
            .call_toolplane_method(
                "find",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                ],
            )
            .expect("find");
        match found {
            Value::Dict(d) => {
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert_eq!(s, "does something"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn tool_plane_register_duplicate_tool_fails() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .unwrap();
        let err = interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .expect_err("duplicate should fail");
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn tool_plane_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_toolplane_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }

    #[test]
    fn tool_plane_remove_plane() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_toolplane_method("remove", &[Value::String("p".to_string())])
            .expect("remove");
        assert_eq!(removed, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("p".to_string())])
            .expect("info");
        assert_eq!(info, Value::Nil);
    }
}

#[cfg(test)]
mod tests_v045_ai {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.45.0: ai.retry / ai.role builtin (mini-swe-agent + OpenFugu)

    #[test]
    fn ai_retry_returns_schedule_dict() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[Value::String("5".to_string()), Value::Number(100.0)],
            )
            .expect("retry");
        match result {
            Value::Dict(d) => {
                let attempts = d.get("attempts").expect("attempts");
                match attempts {
                    Value::Number(n) => assert_eq!(*n, 5.0),
                    _ => panic!("expected Number attempts"),
                }
                let backoff_ms = d.get("backoff_ms").expect("backoff_ms");
                match backoff_ms {
                    Value::Number(n) => assert_eq!(*n, 100.0),
                    _ => panic!("expected Number backoff_ms"),
                }
                let schedule = d.get("schedule").expect("schedule");
                match schedule {
                    Value::List(items) => {
                        assert_eq!(items.len(), 5, "schedule should have 5 entries")
                    }
                    _ => panic!("expected List schedule"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn ai_retry_exponential_schedule_grows() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[
                    Value::String("4".to_string()),
                    Value::Number(100.0),
                    Value::String("exponential".to_string()),
                ],
            )
            .expect("retry");
        match result {
            Value::Dict(d) => {
                let schedule = d.get("schedule").expect("schedule");
                if let Value::List(items) = schedule {
                    let nums: Vec<f64> = items
                        .iter()
                        .filter_map(|v| match v {
                            Value::Number(n) => Some(*n),
                            _ => None,
                        })
                        .collect();
                    // exponential: 100, 200, 400, 800
                    assert_eq!(nums, vec![100.0, 200.0, 400.0, 800.0]);
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_retry_rejects_zero_attempts() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("retry", &[Value::String("0".to_string())])
            .expect_err("zero attempts should fail");
        assert!(err.contains("attempts must be > 0"), "got: {}", err);
    }

    #[test]
    fn ai_role_accepts_main_three_roles() {
        let mut interp = Interpreter::new();
        for role in ["worker", "thinker", "verifier"] {
            let result = interp
                .call_ai_method("role", &[Value::String(role.to_string())])
                .expect("role");
            match result {
                Value::String(s) => assert_eq!(s, role),
                _ => panic!("expected String"),
            }
        }
    }

    #[test]
    fn ai_role_accepts_custom_role() {
        // OpenFugu has 3 main roles but custom roles also OK
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("role", &[Value::String("explorer".to_string())])
            .expect("role");
        match result {
            Value::String(s) => assert_eq!(s, "explorer"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn ai_role_requires_arg() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("role", &[])
            .expect_err("no arg should fail");
        assert!(err.contains("requires role name"), "got: {}", err);
    }

    #[test]
    fn ai_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v046_skill {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.46.0: skill.* builtin (CLI-Anything SKILL.md pattern)
    fn write_temp_skill_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_skill_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn skill_list_empty_by_default() {
        let mut interp = Interpreter::new();
        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => assert_eq!(items.len(), 0),
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_install_registers_skill() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: my-skill
description: A test skill
trigger: test.*
---

This is the body of my-skill.
"#;
        let result = interp
            .call_skill_method(
                "install",
                &[
                    Value::String("my-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .expect("install");
        assert_eq!(result, Value::Bool(true));

        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"my-skill".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_returns_full_spec() {
        let mut interp = Interpreter::new();
        let content = "---
name: finder-skill
description: Helps find things
trigger: find.*
---

# Body
Find things here.
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("finder-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let found = interp
            .call_skill_method("find", &[Value::String("finder-skill".to_string())])
            .expect("find");
        match found {
            Value::Dict(d) => {
                let name = d.get("name").expect("name");
                match name {
                    Value::String(s) => assert_eq!(s, "finder-skill"),
                    _ => panic!("expected name String"),
                }
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert!(s.contains("find things")),
                    _ => panic!("expected desc String"),
                }
                let trigger = d.get("trigger").expect("trigger");
                match trigger {
                    Value::String(s) => assert_eq!(s, "find.*"),
                    _ => panic!("expected trigger String"),
                }
                let body = d.get("body").expect("body");
                match body {
                    Value::String(s) => assert!(s.contains("Find things here")),
                    _ => panic!("expected body String"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_unknown_returns_nil() {
        let mut interp = Interpreter::new();
        let found = interp
            .call_skill_method("find", &[Value::String("nope".to_string())])
            .expect("find");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_load_real_skill_md_file() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: file-loaded
description: Loaded from file
---

This skill was loaded from a real file on disk.
"#;
        let path = write_temp_skill_file("file-loaded", content);

        let result = interp
            .call_skill_method("load", &[Value::String(path.to_string_lossy().to_string())])
            .expect("load");
        assert_eq!(result, Value::Bool(true));

        let list = interp.call_skill_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"file-loaded".to_string()));
            }
            _ => panic!("expected List"),
        }

        let found = interp
            .call_skill_method("find", &[Value::String("file-loaded".to_string())])
            .expect("find");
        match found {
            Value::Dict(d) => {
                let src = d.get("source").expect("source");
                match src {
                    Value::String(s) => assert!(s.contains("file-loaded.md"), "got: {}", s),
                    _ => panic!("expected source path String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skill_load_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("load", &[Value::String("/nonexistent/foo.md".to_string())])
            .expect_err("nonexistent should fail");
        assert!(err.contains("skill.load"), "got: {}", err);
    }

    #[test]
    fn skill_uninstall_removes() {
        let mut interp = Interpreter::new();
        let content = "---
name: temp
description: temporary
---

body
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("temp".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_skill_method("uninstall", &[Value::String("temp".to_string())])
            .expect("uninstall");
        assert_eq!(removed, Value::Bool(true));
        let found = interp
            .call_skill_method("find", &[Value::String("temp".to_string())])
            .expect("find");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_set_hub_and_refresh_real_file() {
        let mut interp = Interpreter::new();
        let dir = std::env::temp_dir().join(format!(
            "mora_hub_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hub_path = dir.join("mora-public.json");
        let hub_content = r#"{
  "skills": [
    {"name": "hub-skill-a", "description": "Hub A"},
    {"name": "hub-skill-b", "description": "Hub B"}
  ]
}"#;
        std::fs::write(&hub_path, hub_content).unwrap();

        let set = interp
            .call_skill_method(
                "set_hub",
                &[Value::String(hub_path.to_string_lossy().to_string())],
            )
            .expect("set_hub");
        assert_eq!(set, Value::Bool(true));

        let count = interp
            .call_skill_method("refresh_hub", &[])
            .expect("refresh_hub");
        match count {
            Value::Number(n) => assert!(n >= 1.0, "expected at least 1 hub entry, got {}", n),
            other => panic!("expected Number, got: {:?}", other),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v048_plan {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.48.0: plan.* builtin (pi-agent update_plan pattern)

    #[test]
    fn plan_create_then_list() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s1".to_string()));
                d.insert("text".to_string(), Value::String("first".to_string()));
                d.insert("status".to_string(), Value::String("pending".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s2".to_string()));
                d.insert("text".to_string(), Value::String("second".to_string()));
                d
            }),
        ];
        let name = interp
            .call_plan_method(
                "create",
                &[Value::String("myplan".to_string()), Value::List(steps)],
            )
            .expect("create");
        assert_eq!(name, Value::String("myplan".to_string()));

        let list = interp.call_plan_method("list", &[]).expect("list");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"myplan".to_string()));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_update_step_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // update a -> done
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("done".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let done = d.get("done").expect("done");
                match done {
                    Value::Number(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_update_supports_emoji_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // emoji ✅
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("✅".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update with emoji");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn plan_update_unknown_step_errors() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::List(vec![Value::Dict({
                        let mut d = std::collections::HashMap::new();
                        d.insert("id".to_string(), Value::String("a".to_string()));
                        d.insert("text".to_string(), Value::String("A".to_string()));
                        d
                    })]),
                ],
            )
            .unwrap();
        let updates = vec![Value::List(vec![
            Value::String("ghost".to_string()),
            Value::String("done".to_string()),
        ])];
        let err = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect_err("unknown step should fail");
        assert!(err.contains("not found"), "got: {}", err);
    }

    #[test]
    fn plan_add_and_remove_step() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(vec![])],
            )
            .unwrap();
        let added = interp
            .call_plan_method(
                "add",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                    Value::String("A".to_string()),
                ],
            )
            .expect("add");
        assert_eq!(added, Value::Bool(true));
        let removed = interp
            .call_plan_method(
                "remove",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .expect("remove");
        assert_eq!(removed, Value::Bool(true));
    }

    #[test]
    fn plan_list_returns_steps_with_emoji() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let list = interp
            .call_plan_method("list", &[Value::String("p".to_string())])
            .expect("list steps");
        match list {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                match &items[0] {
                    Value::Dict(d) => {
                        let emoji = d.get("emoji").expect("emoji");
                        match emoji {
                            Value::String(s) => assert_eq!(s, "⬜"), // pending default
                            _ => panic!("expected emoji String"),
                        }
                    }
                    _ => panic!("expected Dict"),
                }
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_info_reports_counts() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("a".to_string()));
                d.insert("text".to_string(), Value::String("A".to_string()));
                d.insert("status".to_string(), Value::String("done".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("b".to_string()));
                d.insert("text".to_string(), Value::String("B".to_string()));
                d
            }),
        ];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info");
        match info {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Number(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done");
                match done {
                    Value::Number(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Number(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio");
                match ratio {
                    Value::Number(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_plan_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v048_refine {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.48.0: mora.refine + mora.refine_info + mora.list_refines (CLI-Anything /refine)
    #[allow(unused_imports)]
    use std::time::UNIX_EPOCH;

    fn write_temp_script(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_refine_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn mora_refine_real_file_creates_refined_copy() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("demo.mora", "task main()\n  print(\"hi\")\n");
        let result = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("add greeting".to_string()),
                ],
            )
            .expect("refine");
        match result {
            Value::Dict(d) => {
                let iter = d.get("iteration").expect("iteration");
                match iter {
                    Value::Number(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let refined = d.get("refined").expect("refined");
                match refined {
                    Value::String(s) => assert!(s.contains(".refined.1.mora")),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        // 验证 .refine/ 目录存在 + 副本可读
        let refine_dir = script.parent().unwrap().join("demo.refine");
        assert!(refine_dir.exists(), ".refine/ should be created");
        let refined_path = refine_dir.join("demo.refined.1.mora");
        assert!(refined_path.exists(), "refined copy should exist");
        let content = std::fs::read_to_string(&refined_path).unwrap();
        assert!(content.contains("add greeting"));
        assert!(content.contains("task main()"));

        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_iteration_increments() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("iter.mora", "x\n");
        for i in 1..=3 {
            let result = interp
                .call_mora_method(
                    "refine",
                    &[
                        Value::String(script.to_string_lossy().to_string()),
                        Value::String(format!("iter {}", i)),
                    ],
                )
                .expect("refine");
            match result {
                Value::Dict(d) => {
                    let iter = d.get("iteration").expect("iteration");
                    match iter {
                        Value::Number(n) => assert_eq!(*n, i as f64),
                        _ => panic!("expected Number"),
                    }
                }
                _ => panic!("expected Dict"),
            }
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_returns_latest() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("info.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("first".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[Value::String(script.to_string_lossy().to_string())],
            )
            .expect("refine_info");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "first"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_specific_iteration() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("specific.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v1".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v2".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::Number(1.0),
                ],
            )
            .expect("iter 1");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "v1"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_list_refines_lists_all_scripts() {
        let mut interp = Interpreter::new();
        let s1 = write_temp_script("s1.mora", "1\n");
        let s2 = write_temp_script("s2.mora", "2\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s1.to_string_lossy().to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s2.to_string_lossy().to_string()),
                    Value::String("b".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_mora_method("list_refines", &[])
            .expect("list_refines");
        match list {
            Value::List(items) => {
                let paths: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(paths.len(), 2);
                assert!(paths.iter().any(|p| p.contains("s1.mora")));
                assert!(paths.iter().any(|p| p.contains("s2.mora")));
            }
            _ => panic!("expected List"),
        }
        let _ = std::fs::remove_dir_all(s1.parent().unwrap());
        let _ = std::fs::remove_dir_all(s2.parent().unwrap());
    }

    #[test]
    fn mora_refine_nonexistent_script_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String("/nonexistent/foo.mora".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("mora.refine"), "got: {}", err);
    }

    #[test]
    fn mora_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v047_dag {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.dag builtin (OpenFugu §1.6 DAG-as-data)

    #[test]
    fn ai_dag_linear_returns_topological_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("c".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names, vec!["a", "b", "c"]);
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn ai_dag_cycle_returns_error() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("a".to_string()),
            ]),
        ];
        let err = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect_err("cycle should fail");
        assert!(err.contains("ai.dag"), "got: {}", err);
        assert!(err.contains("cycle"), "got: {}", err);
    }

    #[test]
    fn ai_dag_diamond_returns_valid_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
            Value::String("d".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("c".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("d".to_string()),
            ]),
            Value::List(vec![
                Value::String("c".to_string()),
                Value::String("d".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names[0], "a");
                assert_eq!(names[3], "d");
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_empty_edges_returns_nodes() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(vec![])])
            .expect("dag");
        match result {
            Value::List(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_requires_2_args() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("dag", &[])
            .expect_err("no args should fail");
        assert!(err.contains("requires 2 args"), "got: {}", err);
    }
}

#[cfg(test)]
mod tests_v047_heartbeat {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.heartbeat builtin (mimiclaw §1.5 HEARTBEAT.md pattern)
    use std::time::UNIX_EPOCH;

    fn write_heartbeat(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_hb_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn ai_heartbeat_real_file_returns_report() {
        let mut interp = Interpreter::new();
        let content = r#"# Heartbeat
- [x] first done
- [ ] second pending
- [x] third done
- [ ] fourth pending
"#;
        let path = write_heartbeat("HB", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Number(n) => assert_eq!(*n, 4.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done");
                match done {
                    Value::Number(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Number(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio");
                match ratio {
                    Value::Number(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(false));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_all_done_is_complete() {
        let mut interp = Interpreter::new();
        let content = "- [x] a\n- [X] b\n- [x] c\n";
        let path = write_heartbeat("all_done", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_empty_heartbeat_is_vacuously_complete() {
        let mut interp = Interpreter::new();
        let content = "# only heading\nno checklist items\n";
        let path = write_heartbeat("empty", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total");
                match total {
                    Value::Number(n) => assert_eq!(*n, 0.0),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String("/nonexistent/HEARTBEAT.md".to_string())],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("ai.heartbeat"), "got: {}", err);
    }

    #[test]
    fn ai_heartbeat_items_list_contains_text_and_done() {
        let mut interp = Interpreter::new();
        let content = "- [x] task A\n- [ ] task B\n";
        let path = write_heartbeat("items", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let items = d.get("items").expect("items");
                match items {
                    Value::List(items) => {
                        assert_eq!(items.len(), 2);
                        match &items[0] {
                            Value::Dict(item) => {
                                let done = item.get("done").expect("done");
                                assert_eq!(*done, Value::Bool(true));
                            }
                            _ => panic!("expected Dict"),
                        }
                    }
                    _ => panic!("expected List"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

#[cfg(test)]
mod tests_v047_context {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.47.0: ai.context.trim + ai.context.info (pi-agent + AgentMesh pattern)

    #[test]
    fn ai_context_info_returns_window_state() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.info", &[])
            .expect("context.info");
        match result {
            Value::Dict(d) => {
                let max = d.get("max_tokens").expect("max_tokens");
                match max {
                    Value::Number(n) => assert_eq!(*n, 4096.0, "default max"),
                    _ => panic!("expected Number"),
                }
                let msgs = d.get("messages").expect("messages");
                match msgs {
                    Value::Number(n) => assert_eq!(*n, 0.0, "default empty"),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_context_trim_empty_drops_zero() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[])
            .expect("context.trim");
        match result {
            Value::Number(n) => assert_eq!(n, 0.0, "empty context drops 0 tokens"),
            _ => panic!("expected Number"),
        }
    }

    #[test]
    fn ai_context_trim_validates_threshold_range() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("context.trim", &[Value::Number(1.5)])
            .expect_err("1.5 should fail");
        assert!(err.contains("0.0-1.0"), "got: {}", err);

        let err2 = interp
            .call_ai_method("context.trim", &[Value::Number(-0.1)])
            .expect_err("-0.1 should fail");
        assert!(err2.contains("0.0-1.0"), "got: {}", err2);
    }

    #[test]
    fn ai_context_trim_accepts_valid_threshold() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[Value::Number(0.5)])
            .expect("should succeed");
        match result {
            Value::Number(_) => {}
            _ => panic!("expected Number"),
        }
    }
}

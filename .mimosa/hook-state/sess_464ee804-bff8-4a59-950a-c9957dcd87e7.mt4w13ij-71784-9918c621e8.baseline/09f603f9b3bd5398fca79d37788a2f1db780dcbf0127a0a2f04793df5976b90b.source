//! v0.75.51: sandbox.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_sandbox_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "mode" => {
                let policy = &self.sandbox.sandbox;
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
                Ok(Value::Bool(
                    self.sandbox.sandbox.check_builtin(&name).is_ok(),
                ))
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
                Ok(Value::Bool(self.sandbox.sandbox.check_path(&path).is_ok()))
            }
            // v0.42.0: sandbox.key { file.read, web.fetch } — issue capability token
            // Returns: token handle as Value::Float(token_id)
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
                    .sandbox
                    .capabilities
                    .issue(allowed, ttl)
                    .map_err(|e| format!("sandbox.key: issue failed: {}", e))?;
                Ok(Value::Float(token_id as f64))
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
                    Value::Float(n) => *n as u64,
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
                    self.sandbox
                        .sandbox
                        .capabilities
                        .check(token_id, cap)
                        .is_ok(),
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
                    Value::Float(n) => *n as u64,
                    Value::Int(i) => *i as u64,
                    _ => {
                        return Err("sandbox.revoke: token_id must be a number".to_string());
                    }
                };
                self.sandbox
                    .sandbox
                    .capabilities
                    .revoke(token_id)
                    .map_err(|e| format!("sandbox.revoke: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.0: sandbox.token_count() — diagnostic
            "token_count" => Ok(Value::Float(
                self.sandbox.sandbox.capabilities.token_count() as f64,
            )),
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
                self.persist
                    .audit_sink
                    .write(event)
                    .map_err(|e| format!("sandbox.audit_emit: write failed: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.1: sandbox.audit_flush() — flush audit sink to disk
            "audit_flush" => {
                self.persist
                    .audit_sink
                    .flush()
                    .map_err(|e| format!("sandbox.audit_flush: {}", e))?;
                Ok(Value::Bool(true))
            }
            // v0.42.1: sandbox.audit_verify() — verify hash chain (returns true / error string)
            "audit_verify" => match self.persist.audit_sink.verify_chain() {
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
                        Value::Float(v) => spec.limits.cpu_cores = Some(*v as u32),
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
                        Value::Float(v) => spec.limits.memory_mb = Some(*v as u64),
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

                *self.sandbox.container.lock().expect("container poisoned") = Some(handle);
                Ok(Value::Float(id_hash as f64))
            }
            // v0.44.0: sandbox.container_exec(cmd, args...) — run cmd INSIDE container via docker exec
            // Returns: Dict{exit_code, stdout, stderr, elapsed_ms}
            "container_exec" => {
                let guard = self.sandbox.container.lock().expect("container poisoned");
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
                d.insert("exit_code".to_string(), Value::Float(code as f64));
                d.insert("stdout".to_string(), Value::String(stdout));
                d.insert("stderr".to_string(), Value::String(stderr));
                d.insert(
                    "elapsed_ms".to_string(),
                    Value::Float(handle.elapsed().as_millis() as f64),
                );
                Ok(Value::Dict(d))
            }
            // v0.44.0: sandbox.container_info() — diagnostic, returns Dict (container_id, name, backend, mounts)
            "container_info" => {
                let guard = self.sandbox.container.lock().expect("container poisoned");
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
                            Value::Float(handle.spec.mounts.len() as f64),
                        );
                        d.insert(
                            "elapsed_ms".to_string(),
                            Value::Float(handle.elapsed().as_millis() as f64),
                        );
                        Ok(Value::Dict(d))
                    }
                    None => Ok(Value::Nil),
                }
            }
            // v0.44.0: sandbox.container_clear() — REAL docker rm -f, then clear handle
            "container_clear" => {
                let mut guard = self.sandbox.container.lock().expect("container poisoned");
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
}

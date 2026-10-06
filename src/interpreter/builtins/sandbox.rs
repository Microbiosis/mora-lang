//! v0.75.51: sandbox.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

/// v0.104.6 D283：用户传入数值的**唯一**转换入口（本文件内）。
///
/// 修前本文件有 4 处直接 `as u64` / `as u32` 吃用户传进来的 `Value`：
///
/// ```text
/// Value::Float(n) => *n as u64,   // Float(-1.0) as u64 —— **饱和成 0**
/// Value::Int(i)   => *i as u64,   // Int(-1)   as u64 —— **回绕成 u64::MAX**
/// ```
///
/// 后果是**一个负数被静默换成一个「看起来合法」的 id**。实测
/// `sandbox.revoke(-1)` 报的是 `capability token 0 not found (revoked?)`
/// —— `-1` 饱和成了 0，于是去查 token 0，错误信息把排查方向引到 token 0，
/// 而真正的问题（传了负数）被完全掩盖。
///
/// 同样的形状也让 `containerize` 的 `cpu_cores` / `memory_mb` 把负数变成
/// `Some(0)`（0 核 / 0 内存），比 `None`（不限）**更危险**。
///
/// 改走 [`crate::flow::value_as_usize`]（D246 立的收口）：负数 / `NaN` /
/// `±inf` 一律返回 `None`，由调用方**报错**——正是该收口文档里写的
/// 「不替它猜」。
fn arg_nonneg_u64(v: &Value, what: &str) -> Result<u64, String> {
    crate::flow::value_as_usize(v)
        .map(|n| n as u64)
        .ok_or_else(|| format!("{what} must be a non-negative integer, got {v:?}"))
}

/// 同 [`arg_nonneg_u64`]，但额外要求落在 `u32` 范围内（`cpu_cores` 用）。
///
/// `value_as_usize` 在 64 位上给到 `u64`，直接 `as u32` 会**静默截断**
/// （例如 `4294967297` → 1），所以这里显式校验上界。
fn arg_nonneg_u32(v: &Value, what: &str) -> Result<u32, String> {
    let n = crate::flow::value_as_usize(v)
        .ok_or_else(|| format!("{what} must be a non-negative integer, got {v:?}"))?;
    u32::try_from(n).map_err(|_| format!("{what} must be <= 4294967295, got {n}"))
}

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
                let token_id = arg_nonneg_u64(&args[0], "sandbox.check_call: token_id")?;
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
                let token_id = arg_nonneg_u64(&args[0], "sandbox.revoke: token_id")?;
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
                // v0.104.6 D152：此前 `if let Some(Value::List(..))` 无 else 分支 ——
                // 传错类型即**静默当成没传**（mounts 悄悄消失）。同函数的
                // `cpu_cores` / `memory_mb` 一直都有 else 报错（见下），此处是对齐它们。
                match args.get(1) {
                    None | Some(Value::Nil) => {}
                    Some(Value::List(mounts)) => {
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
                    Some(_) => {
                        return Err(
                            "sandbox.containerize: mounts must be a list of strings".to_string()
                        );
                    }
                }

                // network (可选, arg 2)
                // v0.104.6 D152：同 mounts —— 传错类型曾**静默保持默认网络模式**。
                if let Some(net_str) = optional_str_arg(args, 2, "sandbox.containerize", "network")?
                {
                    spec.network =
                        crate::sandbox::NetworkMode::parse(&net_str).ok_or_else(|| {
                            format!("sandbox.containerize: unknown network '{}'", net_str)
                        })?;
                }

                // cpu_cores (可选, arg 3)
                // v0.104.6 D283：修前 `*v as u32` 使负数饱和成 `Some(0)`
                // （0 核），比 `None`（不限）更危险；超 `u32` 上界也会静默截断。
                if let Some(n) = args.get(3) {
                    match n {
                        Value::Nil => {}
                        other => {
                            spec.limits.cpu_cores =
                                Some(arg_nonneg_u32(other, "sandbox.containerize: cpu_cores")?);
                        }
                    }
                }

                // memory_mb (可选, arg 4) —— 同上，负数曾饱和成 `Some(0)`（0 内存）。
                if let Some(n) = args.get(4) {
                    match n {
                        Value::Nil => {}
                        other => {
                            spec.limits.memory_mb =
                                Some(arg_nonneg_u64(other, "sandbox.containerize: memory_mb")?);
                        }
                    }
                }

                // image (可选, arg 5; default alpine:latest)
                // v0.104.6 D152：同 mounts —— 传错类型曾**静默保持默认镜像**。
                // 本轮实测：传 `12345` 时错误最终以「docker daemon unreachable」暴露，
                // 真正的问题（image 类型不对）被完全掩盖，且 exit 1 的归因是错的。
                if let Some(img) = optional_str_arg(args, 5, "sandbox.containerize", "image")? {
                    spec.image = img;
                }

                spec.validate()
                    .map_err(|e| format!("sandbox.containerize: {}", e))?;

                // **REAL spawn** — 真的调用 docker run
                // v0.101: 容器名由本实例的计数器生成（数据流），
                // spawn 是 (spec, name) → handle 的纯转换。
                let name = self.sandbox.next_container_name();
                let handle = crate::sandbox::spawn_container(&spec, &name)
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

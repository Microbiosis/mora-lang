//! v0.75.51: toolplane.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_toolplane_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut reg = self
            .sandbox
            .tool_planes
            .lock()
            .expect("tool_planes poisoned");

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
                            Value::Float(plane.tool_count() as f64),
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
}

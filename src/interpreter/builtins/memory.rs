//! v0.75.51: memory.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_memory_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "store" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.store: requires key")?;
                let value = args.get(1).cloned().unwrap_or(Value::Nil);
                self.registry.memory_store.insert(key, value);
                Ok(Value::Nil)
            }
            "recall" => {
                let key = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.recall: requires key")?;
                Ok(self
                    .registry
                    .memory_store
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Nil))
            }
            "search" => {
                let query = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("memory.search: requires query")?;
                let query_lower = query.to_lowercase();
                let results: Vec<Value> = self
                    .registry
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
                self.registry.memory_store.remove(&key);
                Ok(Value::Nil)
            }
            "clear" => {
                self.registry.memory_store.clear();
                Ok(Value::Nil)
            }
            "size" => Ok(Value::Float(self.registry.memory_store.len() as f64)),
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
                remember_markdown(
                    self.persist.markdown_memory_dir.as_deref(),
                    &category,
                    &text,
                )
                .map_err(|e| format!("memory.remember: {}", e))?;
                // 也写到 memory_store (key=category, value=text) 让 recall 能查到
                self.registry
                    .memory_store
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
                recall_markdown(self.persist.markdown_memory_dir.as_deref(), &category)
                    .map(Value::String)
                    .map_err(|e| format!("memory.recall_markdown: {}", e))
            }
            // v0.43.1: memory.list_markdown() — list all categories
            // Returns: List[String] of category names
            "list_markdown" => {
                list_markdown_categories(self.persist.markdown_memory_dir.as_deref())
                    .map(|cats| Value::List(cats.into_iter().map(Value::String).collect()))
                    .map_err(|e| format!("memory.list_markdown: {}", e))
            }
            "keys" => {
                let keys: Vec<Value> = self
                    .registry
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
                let json = value_to_json(&Value::Dict(self.registry.memory_store.clone()));
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
                        self.registry.memory_store = map;
                        Ok(Value::Bool(true))
                    }
                    Ok(_) => Err("memory.load: file must contain a JSON object".to_string()),
                    Err(e) => Err(format!("memory.load: {}", e)),
                }
            }
            _ => Err(format!("memory has no method: {}", method)),
        }
    }
}

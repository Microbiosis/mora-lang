//! v0.75.54: memory.* builtin 实现 — P7 拆 domain 后补全：markdown 辅助函数
//! (markdown_memory_dir/remember/recall/list + 日期工具) 与 tests_v0431_memory_bus
//! (memory 部分) 从 builtins/mod.rs 迁入。语义与拆分前完全一致（纯搬移）。

use super::*;
use crate::value::Value;

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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
        interp.persist.markdown_memory_dir = Some(dir.clone());
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
}

//! v0.25: record 单元测试（录制/重放/审计/快照回路）。

use crate::record::*;
use std::env;
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = env::temp_dir();
    p.push(format!(
        "mora_record_test_{}_{}.jsonl",
        name,
        std::process::id()
    ));
    p
}

#[test]
fn recorder_off_is_noop() {
    let mut r = Recorder::new_off();
    assert!(r.mode().is_off());
    r.record_ai_chat(
        "gpt-4o".to_string(),
        "hi".to_string(),
        "hello".to_string(),
        1,
        1,
        100,
        None,
        "test_sig".to_string(),
    );
    assert_eq!(r.events().len(), 0); // off 模式不录制
}

/// v0.104.6 D174: `RecordMemory` —— 录事件但**不落盘**。
///
/// `mora snapshot` 只需要 `events()` 拿去与基线比对，不需要一份 JSONL 录像。
/// 与 `new_record` 的区别就是「没有目标文件」，故：
/// - `is_record()` 必须为真（否则所有 `record_*` 的门控会挡掉事件）；
/// - `save()` 必须是 no-op（不能凭空造文件/目录）。
#[test]
fn record_memory_records_but_never_writes() {
    let mut r = Recorder::new_record_memory();
    // 门控：off 会挡住一切事件 —— RecordMemory 绝不能落进 off 那档。
    assert!(r.mode().is_record(), "RecordMemory 必须算 record 模式");
    assert!(!r.mode().is_off(), "RecordMemory 不是 off");
    assert!(!r.mode().is_replay(), "RecordMemory 不是 replay");

    r.record_ai_chat(
        "gpt-4o".to_string(),
        "hello".to_string(),
        "world".to_string(),
        5,
        7,
        123,
        None,
        "test_sig".to_string(),
    );
    assert_eq!(r.events().len(), 1, "RecordMemory 必须真的累积事件");

    // save() 是 no-op：不报错、不消费事件、也不往磁盘写任何东西。
    //
    // 判据用**独占目录**而不是 `%TEMP%` 的文件计数 —— 后者是脆判据：
    // 单独跑本测试时通过，全量套件里红，因为**并行测试**同时在
    // `%TEMP%` 建文件，计数会漂。（D171 教训的又一次自踩。）
    let dir = env::temp_dir().join(format!(
        "mora_recmem_{}_{}",
        std::process::id(),
        Recorder::now_ms()
    ));
    fs::create_dir_all(&dir).expect("建独占目录");
    let count = || fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(count(), 0, "独占目录初始应为空");

    r.save().expect("RecordMemory 的 save 不该是错误");
    assert_eq!(count(), 0, "RecordMemory 的 save() 绝不能往磁盘写东西");
    assert_eq!(r.events().len(), 1, "save() 不该消费/清空事件");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn record_roundtrip() {
    let path = tmp_path("roundtrip");
    let _ = fs::remove_file(&path);

    let mut r = Recorder::new_record(path.clone()).unwrap();
    assert!(r.mode().is_record());
    r.record_ai_chat(
        "gpt-4o".to_string(),
        "hello".to_string(),
        "world".to_string(),
        5,
        7,
        123,
        None,
        "test_sig".to_string(),
    );
    r.record_web_fetch(
        "https://example.com/api".to_string(),
        "GET".to_string(),
        200,
        1024,
        45,
        None,
        "test_sig".to_string(),
    );
    r.record_note("test note".to_string());
    r.save().unwrap();

    // load + replay
    let mut r2 = Recorder::new_replay(path.clone()).unwrap();
    assert!(r2.mode().is_replay());
    assert_eq!(r2.events().len(), 3);
    // lookup ai.chat
    let resp = r2.lookup_ai_chat("gpt-4o", "hello", "test_sig");
    assert!(resp.is_some());
    let resp = resp.unwrap();
    assert_eq!(resp.response, "world");
    assert_eq!(resp.tokens_in, 5);
    // lookup web.fetch
    let wresp = r2.lookup_web_fetch("https://example.com/api", "test_sig");
    assert!(wresp.is_some());
    let wresp = wresp.unwrap();
    assert_eq!(wresp.status, Some(200));
    assert_eq!(wresp.body_len, Some(1024));

    let _ = fs::remove_file(&path);
}

#[test]
fn replay_missing_returns_none() {
    let path = tmp_path("missing");
    let _ = fs::remove_file(&path);
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_ai_chat(
        "gpt-4o".to_string(),
        "first".to_string(),
        "one".to_string(),
        1,
        1,
        50,
        None,
        "test_sig".to_string(),
    );
    r.save().unwrap();

    let mut r2 = Recorder::new_replay(path.clone()).unwrap();
    // 询问不同 prompt → 找不到
    let resp = r2.lookup_ai_chat("gpt-4o", "second", "test_sig");
    assert!(resp.is_none());
    // 询问不同 model → 找不到
    let resp = r2.lookup_ai_chat("gpt-4o-mini", "first", "test_sig");
    assert!(resp.is_none());
    // 询问 web.fetch 不存在 url
    let resp = r2.lookup_web_fetch("https://nope.com", "test_sig");
    assert!(resp.is_none());

    let _ = fs::remove_file(&path);
}

#[test]
fn hash_prompt_deterministic() {
    assert_eq!(hash_prompt("hello"), hash_prompt("hello"));
    assert_ne!(hash_prompt("hello"), hash_prompt("world"));
    assert_eq!(hash_prompt("hello").len(), 16); // 64-bit hex = 16 chars
}

#[test]
fn diff_identical_recordings() {
    let path_a = tmp_path("diff_a");
    let path_b = tmp_path("diff_b");
    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);

    let mut a = Recorder::new_record(path_a.clone()).unwrap();
    a.record_ai_chat(
        "m".into(),
        "p".into(),
        "r".into(),
        1,
        1,
        10,
        None,
        "test_sig".to_string(),
    );
    a.save().unwrap();

    let mut b = Recorder::new_record(path_b.clone()).unwrap();
    b.record_ai_chat(
        "m".into(),
        "p".into(),
        "r".into(),
        1,
        1,
        10,
        None,
        "test_sig".to_string(),
    );
    b.save().unwrap();

    let ra = Recorder::new_replay(path_a.clone()).unwrap();
    let rb = Recorder::new_replay(path_b.clone()).unwrap();
    let diff = diff_recordings(ra.events(), rb.events());
    assert_eq!(diff.len(), 1);
    assert!(matches!(diff[0], DiffLine::Identical(1, _)));

    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);
}

#[test]
fn diff_changed_response() {
    let path_a = tmp_path("diff_chg_a");
    let path_b = tmp_path("diff_chg_b");
    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);

    let mut a = Recorder::new_record(path_a.clone()).unwrap();
    a.record_ai_chat(
        "m".into(),
        "p".into(),
        "old response".into(),
        1,
        1,
        10,
        None,
        "test_sig".to_string(),
    );
    a.save().unwrap();

    let mut b = Recorder::new_record(path_b.clone()).unwrap();
    b.record_ai_chat(
        "m".into(),
        "p".into(),
        "new response longer".into(),
        2,
        2,
        20,
        None,
        "test_sig".to_string(),
    );
    b.save().unwrap();

    let ra = Recorder::new_replay(path_a.clone()).unwrap();
    let rb = Recorder::new_replay(path_b.clone()).unwrap();
    let diff = diff_recordings(ra.events(), rb.events());
    assert_eq!(diff.len(), 1);
    assert!(matches!(diff[0], DiffLine::Changed(1, _, _)));

    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);
}

#[test]
fn diff_only_in_b() {
    let path_a = tmp_path("only_a");
    let path_b = tmp_path("only_b");
    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);

    let a = Recorder::new_record(path_a.clone()).unwrap();
    a.save().unwrap(); // empty

    let mut b = Recorder::new_record(path_b.clone()).unwrap();
    b.record_ai_chat(
        "m".into(),
        "p".into(),
        "r".into(),
        1,
        1,
        10,
        None,
        "test_sig".to_string(),
    );
    b.save().unwrap();

    let ra = Recorder::new_replay(path_a.clone()).unwrap();
    let rb = Recorder::new_replay(path_b.clone()).unwrap();
    let diff = diff_recordings(ra.events(), rb.events());
    assert_eq!(diff.len(), 1);
    assert!(matches!(diff[0], DiffLine::OnlyInB(1, _)));

    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);
}

#[test]
fn list_recordings_empty_dir() {
    let mut dir = env::temp_dir();
    dir.push(format!("mora_list_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let result = list_recordings(&dir);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn list_recordings_finds_files() {
    let mut dir = env::temp_dir();
    dir.push(format!("mora_list_test2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    // 创建一个录制文件
    let mut path = dir.clone();
    path.push("test-rec.jsonl");
    let mut r = Recorder::new_record(path).unwrap();
    r.record_ai_chat(
        "m".into(),
        "p".into(),
        "r".into(),
        1,
        1,
        10,
        None,
        "test_sig".to_string(),
    );
    r.save().unwrap();

    let result = list_recordings(&dir).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "test-rec");
    assert_eq!(result[0].event_count, 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compute_stats_basic() {
    let path = tmp_path("stats");
    let _ = fs::remove_file(&path);
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_ai_chat(
        "gpt-4o".into(),
        "p".into(),
        "r".into(),
        100,
        50,
        200,
        None,
        "test_sig".to_string(),
    );
    r.record_ai_chat(
        "gpt-4o".into(),
        "p2".into(),
        "r2".into(),
        200,
        100,
        300,
        None,
        "test_sig".to_string(),
    );
    r.record_web_fetch(
        "https://x.com".into(),
        "GET".into(),
        200,
        1024,
        50,
        None,
        "test_sig".to_string(),
    );
    r.record_note("test".into());
    r.save().unwrap();

    let r2 = Recorder::new_replay(path.clone()).unwrap();
    let stats = compute_stats(r2.events());
    assert_eq!(stats.total_events, 4);
    assert_eq!(stats.ai_chat_count, 2);
    assert_eq!(stats.web_fetch_count, 1);
    assert_eq!(stats.note_count, 1);
    assert_eq!(stats.total_tokens_in, 300);
    assert_eq!(stats.total_tokens_out, 150);
    assert_eq!(stats.min_latency_ms, 50);
    assert_eq!(stats.max_latency_ms, 300);
    assert_eq!(stats.models, vec!["gpt-4o"]);

    let _ = fs::remove_file(&path);
}

#[test]
fn compute_stats_empty() {
    let stats = compute_stats(&[]);
    assert_eq!(stats.total_events, 0);
    assert_eq!(stats.total_tokens_in, 0);
}

#[test]
fn export_jsonl_roundtrip() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let jsonl = export_recording(&events, &ExportFormat::Jsonl, "test");
    assert!(jsonl.contains("\"kind\":\"ai.chat\""));
    assert!(jsonl.contains("\"model\":\"m\""));
}

#[test]
fn export_markdown_has_table() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let md = export_recording(&events, &ExportFormat::Markdown, "test");
    assert!(md.contains("# Recording: test"));
    assert!(md.contains("| # | Kind |"));
    assert!(md.contains("ai.chat"));
}

#[test]
fn redact_secrets_masks_sk_key() {
    let input = "api_key=sk-abc123def456ghi789jkl012mno";
    let redacted = redact_secrets(input);
    assert!(redacted.contains("<REDACTED>"));
    assert!(!redacted.contains("sk-abc123"));
}

#[test]
fn redact_secrets_masks_bearer() {
    let input =
        "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0";
    let redacted = redact_secrets(input);
    assert!(redacted.contains("Bearer <REDACTED>"));
    assert!(!redacted.contains("eyJhbGci"));
}

#[test]
fn snapshot_roundtrip() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let snap = create_snapshot("test", &events);
    let jsonl = snapshot_to_jsonl(&snap);
    let restored = snapshot_from_jsonl(&jsonl).unwrap();
    assert_eq!(restored.name, "test");
    assert_eq!(restored.event_summaries.len(), 1);
    assert_eq!(restored.event_summaries[0].kind, "ai.chat");
}

#[test]
fn snapshot_diff_match() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let snap = create_snapshot("test", &events);
    let diffs = diff_snapshot(&snap, &events);
    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0], SnapshotDiff::Match(0)));
}

#[test]
fn snapshot_diff_changed() {
    let events_a = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let events_b = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m2".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 20,
        tokens_out: 10,
        latency_ms: 200,
        error: None,
        arg_signature: "".into(),
    }];
    let snap = create_snapshot("test", &events_a);
    let diffs = diff_snapshot(&snap, &events_b);
    assert!(
        diffs
            .iter()
            .any(|d| matches!(d, SnapshotDiff::EventChanged { .. }))
    );
}

#[test]
fn snapshot_diff_missing_event() {
    let events_a = vec![
        Event::AiChat {
            id: 1,
            ts_ms: 1000,
            model: "m".into(),
            prompt_hash: "h".into(),
            prompt_preview: "p".into(),
            response: "r".into(),
            tokens_in: 10,
            tokens_out: 5,
            latency_ms: 100,
            error: None,
            arg_signature: "".into(),
        },
        Event::Note {
            id: 2,
            ts_ms: 1100,
            message: "note".into(),
        },
    ];
    let events_b = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let snap = create_snapshot("test", &events_a);
    let diffs = diff_snapshot(&snap, &events_b);
    assert!(
        diffs
            .iter()
            .any(|d| matches!(d, SnapshotDiff::EventMissing { .. }))
    );
}

#[test]
fn generate_report_basic() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "p".into(),
        response: "r".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let report = generate_report(
        &events,
        "test",
        Some("fix retry"),
        Some("pytest -q"),
        &[("os", "windows")],
    );
    assert!(report.contains("# Evidence Report: test"));
    assert!(report.contains("fix retry"));
    assert!(report.contains("pytest -q"));
    assert!(report.contains("os=windows"));
    assert!(report.contains("## Audit"));
    assert!(report.contains("## Timeline"));
    assert!(report.contains("## Event Log"));
}

#[test]
fn parse_moraignore_basic() {
    let content = r#"
# comment
field:token_usage
path:request.messages.*.content
pattern:github-token
"#;
    let rules = parse_moraignore(content);
    assert_eq!(rules.len(), 3);
    assert!(matches!(&rules[0], IgnoreRule::Field(f) if f == "token_usage"));
    assert!(matches!(&rules[1], IgnoreRule::Path(p) if p == "request.messages.*.content"));
    assert!(matches!(&rules[2], IgnoreRule::Pattern(p) if p == "github-token"));
}

#[test]
fn audit_recording_clean() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "hello".into(),
        response: "world".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let findings = audit_recording(&events, &[]);
    assert_eq!(findings.len(), 0);
}

#[test]
fn audit_recording_finds_sk_key() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "test".into(),
        response: "api_key=sk-abc123def456ghi789jkl012mno".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let findings = audit_recording(&events, &[]);
    assert!(!findings.is_empty());
    assert_eq!(findings[0].pattern, "openai-api-key");
}

#[test]
fn audit_recording_respects_ignore_rules() {
    let events = vec![Event::AiChat {
        id: 1,
        ts_ms: 1000,
        model: "m".into(),
        prompt_hash: "h".into(),
        prompt_preview: "test".into(),
        response: "api_key=sk-abc123def456ghi789jkl012mno".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        error: None,
        arg_signature: "".into(),
    }];
    let rules = vec![IgnoreRule::Pattern("sk-".to_string())];
    let findings = audit_recording(&events, &rules);
    assert_eq!(findings.len(), 0);
}

#[test]
fn redact_secrets_preserves_normal_text() {
    let input = "Hello world, this is a normal message";
    let redacted = redact_secrets(input);
    assert_eq!(redacted, input);
}

#[test]
fn build_timeline_basic() {
    let events = vec![
        Event::AiChat {
            id: 1,
            ts_ms: 1000,
            model: "m".into(),
            prompt_hash: "h".into(),
            prompt_preview: "p".into(),
            response: "hi".into(),
            tokens_in: 10,
            tokens_out: 5,
            latency_ms: 100,
            error: None,
            arg_signature: "".into(),
        },
        Event::Note {
            id: 2,
            ts_ms: 1100,
            message: "note".into(),
        },
    ];
    let rows = build_timeline(&events);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, "ai.chat");
    assert_eq!(rows[0].tokens, "10+5");
    assert_eq!(rows[1].kind, "note");
}

#[test]
fn new_record_creates_parent_dir() {
    // v0.104.6 D34：原先只删 `nested`，**父目录** `mora_record_test_subdir_<pid>`
    // 留着 —— 每次跑测试漏一个。实测该前缀在 `%TEMP%` 下累积到 578 个。
    // 现在把整个基目录删掉（两次：前置清残留 + 后置清本次产物）。
    let mut base = env::temp_dir();
    base.push(format!("mora_record_test_subdir_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);

    let mut p = base.clone();
    p.push("nested");
    p.push("test.jsonl");

    let r = Recorder::new_record(p.clone());
    assert!(r.is_ok());
    assert!(p.parent().unwrap().exists());

    let _ = fs::remove_dir_all(&base);
}

// v0.76.05: Schema 校验测试——arg_signature 不匹配时返 None

fn schema_test_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mora_schema_test_{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("rec.jsonl")
}

#[test]
fn schema_lookup_ai_chat_mismatch_returns_none() {
    // 录制用签名 A，replay 用签名 B → 应返 None（不报 error）
    let path = schema_test_path("ai_mismatch");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_ai_chat(
        "gpt-4o".into(),
        "hello".into(),
        "world".into(),
        10,
        5,
        100,
        None,
        "ai.chat(model: string, prompt: string) -> string".into(),
    );
    r.save().unwrap();
    let mut r2 = Recorder::new_replay(path).unwrap();
    // 当前签名与录制签名不同 → 返 None
    let resp = r2.lookup_ai_chat(
        "gpt-4o",
        "hello",
        "ai.chat(model: string, prompt: string, options: Dict) -> string",
    );
    assert!(resp.is_none(), "签名不匹配应返 None");
}

#[test]
fn schema_lookup_ai_chat_match_returns_some() {
    // 录制与 replay 用同签名 → 返 Some
    let path = schema_test_path("ai_match");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_ai_chat(
        "gpt-4o".into(),
        "hello".into(),
        "world".into(),
        10,
        5,
        100,
        None,
        "ai.chat(model: string, prompt: string) -> string".into(),
    );
    r.save().unwrap();
    let mut r2 = Recorder::new_replay(path).unwrap();
    let resp = r2.lookup_ai_chat(
        "gpt-4o",
        "hello",
        "ai.chat(model: string, prompt: string) -> string",
    );
    assert!(resp.is_some(), "签名匹配应返 Some");
}

#[test]
fn schema_lookup_web_fetch_mismatch_returns_none() {
    let path = schema_test_path("web_mismatch");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_web_fetch(
        "https://x.com".into(),
        "GET".into(),
        200,
        1024,
        50,
        None,
        "web.fetch(url: string, opts: Dict) -> string".into(),
    );
    r.save().unwrap();
    let mut r2 = Recorder::new_replay(path).unwrap();
    let resp = r2.lookup_web_fetch(
        "https://x.com",
        "web.fetch(url: string, headers: Dict, opts: Dict) -> string",
    );
    assert!(resp.is_none(), "签名不匹配应返 None");
}

#[test]
fn schema_mismatch_pushes_warning() {
    // v0.76.06: 签名漂移时不仅返 None，还 push warning 到 self.warnings
    let path = schema_test_path("warning");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_ai_chat(
        "gpt-4o".into(),
        "hello".into(),
        "world".into(),
        10,
        5,
        100,
        None,
        "ai.chat(model: string, prompt: string) -> string".into(),
    );
    r.save().unwrap();
    let mut r2 = Recorder::new_replay(path).unwrap();
    assert!(r2.warnings.is_empty(), "replay 开始时无 warning");
    let resp = r2.lookup_ai_chat(
        "gpt-4o",
        "hello",
        "ai.chat(model: string, prompt: string, options: Dict) -> string",
    );
    assert!(resp.is_none(), "签名不匹配应返 None");
    assert_eq!(r2.warnings.len(), 1, "签名漂移应 push 1 个 warning");
    assert!(
        r2.warnings[0].contains("签名漂移"),
        "warning 应含「签名漂移」"
    );
    assert!(r2.warnings[0].contains("gpt-4o"), "warning 应含模型名");
}

// ─── v0.83: Msg + StateMutation 事件测试 ───

#[test]
fn record_msg_event() {
    let mut r = Recorder::new_off();
    r.record_msg(
        "user_input".to_string(),
        crate::value::Value::String("hello".to_string()),
        42,
    );
    assert_eq!(r.events().len(), 0, "Off 模式不录制");
}

#[test]
fn record_state_mutation_event() {
    let path = tmp_path("state_mutation");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_state_mutation(
        "x".to_string(),
        crate::value::Value::Int(0),
        crate::value::Value::Int(42),
    );
    assert_eq!(r.events().len(), 1);
    match &r.events()[0] {
        Event::StateMutation { var, old, new, .. } => {
            assert_eq!(var, "x");
            assert_eq!(*old, crate::value::Value::Int(0));
            assert_eq!(*new, crate::value::Value::Int(42));
        }
        other => panic!("expected StateMutation, got {:?}", other),
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn msg_event_serialization_roundtrip() {
    let path = tmp_path("msg_roundtrip");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_msg(
        "channel_a".to_string(),
        crate::value::Value::String("payload".to_string()),
        12345,
    );
    r.save().unwrap();
    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert_eq!(r2.events().len(), 1);
    match &r2.events()[0] {
        Event::Msg {
            channel,
            payload,
            prior_state_hash,
            ..
        } => {
            assert_eq!(channel, "channel_a");
            assert_eq!(
                *payload,
                crate::value::Value::String("payload".to_string()),
                "payload roundtrip 完整 Value JSON"
            );
            assert_eq!(*prior_state_hash, 12345);
        }
        other => panic!("expected Msg, got {:?}", other),
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn state_mutation_event_serialization_roundtrip() {
    let path = tmp_path("sm_roundtrip");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_state_mutation(
        "var1".to_string(),
        crate::value::Value::Nil,
        crate::value::Value::Int(100),
    );
    r.save().unwrap();
    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert_eq!(r2.events().len(), 1);
    match &r2.events()[0] {
        Event::StateMutation { var, old, new, .. } => {
            assert_eq!(var, "var1");
            assert_eq!(*old, crate::value::Value::Nil);
            // v0.84: JSON roundtrip 保持 Int 类型 — Int(100) → "100" → Int(100)
            assert_eq!(*new, crate::value::Value::Int(100));
        }
        other => panic!("expected StateMutation, got {:?}", other),
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn recorder_is_off_check() {
    let r1 = Recorder::new_off();
    assert!(r1.is_off());
    let path = tmp_path("not_off");
    let mut r2 = Recorder::new_record(path.clone()).unwrap();
    assert!(!r2.is_off());
    // 写入一个 event 让 file 存在，然后才能 replay
    r2.record_state_mutation(
        "x".into(),
        crate::value::Value::Nil,
        crate::value::Value::Int(1),
    );
    r2.save().unwrap();
    let r3 = Recorder::new_replay(path.clone()).unwrap();
    assert!(!r3.is_off());
    let _ = fs::remove_file(&path);
}

#[test]
fn timeline_includes_msg_and_state_mutation() {
    let events = vec![
        Event::Msg {
            id: 1,
            ts_ms: 1000,
            channel: "ch".to_string(),
            payload: crate::value::Value::Nil,
            prior_state_hash: 0,
        },
        Event::StateMutation {
            id: 2,
            ts_ms: 1100,
            var: "v".to_string(),
            old: crate::value::Value::Nil,
            new: crate::value::Value::Nil,
        },
    ];
    let rows = crate::record::analysis::build_timeline(&events);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, "msg");
    assert_eq!(rows[0].detail, "ch");
    assert_eq!(rows[1].kind, "state_mutation");
    assert_eq!(rows[1].detail, "v");
}

/// D89 第二处：`event_to_jsonl` 的 `WebFetch` 里 **`method` 漏了 `esc()`**
/// （同函数其余字符串字段都转义）。含 `"` 的 method 会写出畸形 JSONL，
/// 解码器引号配对错位 → 该行被 `load_jsonl` 静默丢弃。
///
/// **当前不可达**：两个生产调用点都硬编码 `"GET"`；但 `record_web_fetch`
/// 是 `pub fn`，属潜在缺陷。此测试直接用公开 API 构造来覆盖它。
#[test]
fn d89_web_fetch_method_is_escaped() {
    let path = tmp_path("d89_method_esc");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    let nasty = r#"GE"T,X\Y"#;
    r.record_web_fetch(
        "https://example.com/a?b=1&c=\"2\"".to_string(),
        nasty.to_string(),
        200,
        10,
        5,
        None,
        "sig".to_string(),
    );
    r.save().unwrap();

    let raw = fs::read_to_string(&path).expect("read jsonl");
    assert_eq!(
        raw.lines().count(),
        1,
        "写入必须恰好一行（未转义的引号会撑破 JSON 结构）：\n{raw}"
    );
    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert_eq!(r2.events().len(), 1, "该行不该被静默丢弃");
    match &r2.events()[0] {
        Event::WebFetch { method, url, .. } => {
            assert_eq!(method, nasty, "method 必须原样往返");
            assert_eq!(url, "https://example.com/a?b=1&c=\"2\"");
        }
        other => panic!("expected WebFetch, got {other:?}"),
    }
    let _ = fs::remove_file(&path);
}

/// v0.104.6 D89：`Msg.payload` / `StateMutation.old|new` 是**原样插入的完整
/// Value JSON**，而解码器是「按 `,` 切分（字符串外）」的简易解析器。
///
/// **Dict 有 ≥2 个键时，`{` 内部的 `,` 也在字符串外** —— 解析器从 payload 中间
/// 切一刀，`fields["payload"]` 只拿到被截断的 `{"a":1`，`json_to_value` 解析失败后
/// `.unwrap_or(Value::Nil)` **静默变成 Nil**，事件其余部分照常加载。
///
/// 症状：**录制 → 重放的数据静默丢失，无任何报错**。既有往返测试
/// （`msg_event_serialization_roundtrip` / state_mutation 那条）用的全是
/// `Value::String` / `Value::Int` **标量**，所以这条路从来没被踩到。
#[test]
fn d89_msg_payload_dict_with_multiple_keys_survives_roundtrip() {
    let mut d = std::collections::HashMap::new();
    d.insert("a".to_string(), crate::value::Value::Int(1));
    d.insert(
        "b".to_string(),
        crate::value::Value::String("two".to_string()),
    );
    let payload = crate::value::Value::Dict(d);

    let path = tmp_path("d89_msg_dict");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_msg("ch".to_string(), payload.clone(), 7);
    r.save().unwrap();

    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert_eq!(r2.events().len(), 1, "事件本身不该丢");
    match &r2.events()[0] {
        Event::Msg {
            channel,
            payload: got,
            ..
        } => {
            assert_eq!(channel, "ch");
            assert_eq!(*got, payload, "多键 Dict payload 必须原样往返");
        }
        other => panic!("expected Msg, got {other:?}"),
    }
    let _ = fs::remove_file(&path);
}

/// 同根因的第二条路径：`StateMutation` 的 `old` / `new`。
#[test]
fn d89_state_mutation_dict_survives_roundtrip() {
    let mut d = std::collections::HashMap::new();
    d.insert("k1".to_string(), crate::value::Value::Int(1));
    d.insert("k2".to_string(), crate::value::Value::Int(2));
    let new = crate::value::Value::Dict(d);

    let path = tmp_path("d89_sm_dict");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_state_mutation("v".to_string(), crate::value::Value::Nil, new.clone());
    r.save().unwrap();

    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert_eq!(r2.events().len(), 1);
    match &r2.events()[0] {
        Event::StateMutation { var, new: got, .. } => {
            assert_eq!(var, "v");
            assert_eq!(*got, new, "多键 Dict new 值必须原样往返");
        }
        other => panic!("expected StateMutation, got {other:?}"),
    }
    let _ = fs::remove_file(&path);
}

/// 同一根因的第三种形态：**List 元素里含 Dict**。`[1,2]` 本身在字符串外也带逗号，
/// 同样会被切断。
#[test]
fn d89_msg_payload_list_of_dicts_survives_roundtrip() {
    let mut d = std::collections::HashMap::new();
    d.insert("a".to_string(), crate::value::Value::Int(1));
    d.insert("b".to_string(), crate::value::Value::Int(2));
    let payload = crate::value::Value::List(
        vec![crate::value::Value::Dict(d), crate::value::Value::Int(9)].into(),
    );

    let path = tmp_path("d89_msg_list");
    let mut r = Recorder::new_record(path.clone()).unwrap();
    r.record_msg("ch".to_string(), payload.clone(), 1);
    r.save().unwrap();

    let r2 = Recorder::new_replay(path.clone()).unwrap();
    match &r2.events()[0] {
        Event::Msg { payload: got, .. } => {
            assert_eq!(*got, payload, "含 Dict 的 List payload 必须原样往返");
        }
        other => panic!("expected Msg, got {other:?}"),
    }
    let _ = fs::remove_file(&path);
}

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
    );
    assert_eq!(r.events().len(), 0); // off 模式不录制
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
    );
    r.record_web_fetch(
        "https://example.com/api".to_string(),
        "GET".to_string(),
        200,
        1024,
        45,
        None,
    );
    r.record_note("test note".to_string());
    r.save().unwrap();

    // load + replay
    let r2 = Recorder::new_replay(path.clone()).unwrap();
    assert!(r2.mode().is_replay());
    assert_eq!(r2.events().len(), 3);
    // lookup ai.chat
    let resp = r2.lookup_ai_chat("gpt-4o", "hello");
    assert!(resp.is_some());
    let resp = resp.unwrap();
    assert_eq!(resp.response, "world");
    assert_eq!(resp.tokens_in, 5);
    // lookup web.fetch
    let wresp = r2.lookup_web_fetch("https://example.com/api");
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
    );
    r.save().unwrap();

    let r2 = Recorder::new_replay(path.clone()).unwrap();
    // 询问不同 prompt → 找不到
    let resp = r2.lookup_ai_chat("gpt-4o", "second");
    assert!(resp.is_none());
    // 询问不同 model → 找不到
    let resp = r2.lookup_ai_chat("gpt-4o-mini", "first");
    assert!(resp.is_none());
    // 询问 web.fetch 不存在 url
    let resp = r2.lookup_web_fetch("https://nope.com");
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
    a.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
    a.save().unwrap();

    let mut b = Recorder::new_record(path_b.clone()).unwrap();
    b.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
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
    b.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
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
    r.record_ai_chat("m".into(), "p".into(), "r".into(), 1, 1, 10, None);
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
    r.record_ai_chat("gpt-4o".into(), "p".into(), "r".into(), 100, 50, 200, None);
    r.record_ai_chat(
        "gpt-4o".into(),
        "p2".into(),
        "r2".into(),
        200,
        100,
        300,
        None,
    );
    r.record_web_fetch("https://x.com".into(), "GET".into(), 200, 1024, 50, None);
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
    let mut p = env::temp_dir();
    p.push(format!("mora_record_test_subdir_{}", std::process::id()));
    p.push("nested");
    p.push("test.jsonl");
    let _ = fs::remove_dir_all(p.parent().unwrap());

    let r = Recorder::new_record(p.clone());
    assert!(r.is_ok());
    assert!(p.parent().unwrap().exists());

    let _ = fs::remove_dir_all(p.parent().unwrap());
}

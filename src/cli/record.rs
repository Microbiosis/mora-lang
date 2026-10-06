//! v0.75.53: record CLI 命令（从 main.rs 拆出，P9）。
//! 共享编译/路径辅助在 super::（cli/mod.rs）。

use super::*;
use crate::record;

pub fn run_record(path: &str, name: &str, opt_level: Option<crate::mir::ssa::OptLevel>) {
    // v0.104.6 D189：别把一个「看起来是选项」的参数当路径去读（错误归因会错）。
    super::reject_option_as_path(path);
    let source = super::read_source(std::path::Path::new(path)).unwrap_or_else(|e| {
        // v0.104.6 D192：把**原因**带出来。此前 `|_|` 把错误整个丢掉，于是
        // 编码错误 / 权限不足 / 「是个目录」全都退化成同一句
        // `failed to read <path>` —— 用户无从判断下一步该做什么。
        eprintln!("record: failed to read {}: {}", path, e);
        process::exit(1);
    });

    // v0.103: 解析失败以可读错误 + 退出码 2 报告（compile_and_opt 返回 Result）。
    let (func, witnesses) = match compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("record: {}", e);
            process::exit(2);
        }
    };

    let type_errors = crate::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("record: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    interpreter.infra_mut().replace_recorder(
        match record::Recorder::new_record(rec_path.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("record: {}", e);
                process::exit(1);
            }
        },
    );
    let mut env = interpreter.take_env();

    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    match crate::mir::vm::run_mir(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut crate::mir::effect::Effects::new(),
    ) {
        Ok(_) => {
            // 执行 main task
            if let Err(e) = crate::mir::vm::run_main_task(
                &func_arc,
                &mut interpreter,
                &mut env,
                &mut crate::mir::effect::Effects::new(),
            ) {
                if let Err(e) = interpreter.infra_mut().recorder().save() {
                    eprintln!("[warn] partial recording save failed: {}", e);
                }
                eprintln!("Runtime error during record: {}", e);
                eprintln!("(partial recording saved)");
                process::exit(1);
            }
            if let Err(e) = interpreter.infra_mut().recorder().save() {
                eprintln!("record: save failed: {}", e);
                process::exit(1);
            }
            let n = interpreter.infra().recorder().events().len();
            println!("✓ recorded {} events -> {}", n, rec_path.display());
        }
        Err(e) => {
            if let Err(e) = interpreter.infra_mut().recorder().save() {
                eprintln!("[warn] partial recording save failed: {}", e);
            }
            eprintln!("Runtime error during record: {}", e);
            eprintln!("(partial recording saved)");
            process::exit(1);
        }
    }
}

/// v0.104.6 D178：把「这份录像里有 N 行没能解析」**说出来**。
///
/// 以前 `load_jsonl` 静默丢弃畸形行，于是每个下游命令都在**不完整数据**
/// 上照常报成功 —— 对 `audit` 尤其致命：密钥扫描器在缺了内容的情况下
/// 依然说「No secrets found」。
///
/// 只**报告**、不改变行为（畸形行仍然跳过，前向兼容是有意保留的）——
/// 但从「静默」变成「显式」。
fn warn_skipped(rec: &record::Recorder) {
    if rec.skipped_lines.is_empty() {
        return;
    }
    let n = rec.skipped_lines.len();
    let total = rec.events().len() + n;
    // 文件名直接从 `Mode::Replay(path)` 取 —— 免得每个调用点各传一个标签，
    // 而标签与实际来源漂移又是一个静默说错话的口子。
    let file = match rec.mode() {
        record::Mode::Replay(p) => p.display().to_string(),
        _ => "<recording>".to_string(),
    };
    eprintln!(
        "[warn] {}: {} of {} line(s) could not be parsed and were SKIPPED — \
         the result below is based on PARTIAL data.",
        file, n, total
    );
    // 逐行列出前几行（限长片段，见 `SkippedLine::excerpt` 的说明）。
    const SHOW: usize = 5;
    for s in rec.skipped_lines.iter().take(SHOW) {
        eprintln!("       line {}: {}", s.line_no, s.excerpt);
    }
    if n > SHOW {
        eprintln!("       … and {} more", n - SHOW);
    }
}

pub fn run_replay(path: &str, name: &str, opt_level: Option<crate::mir::ssa::OptLevel>) {
    // v0.104.6 D189：别把一个「看起来是选项」的参数当路径去读（错误归因会错）。
    super::reject_option_as_path(path);
    let source = super::read_source(std::path::Path::new(path)).unwrap_or_else(|e| {
        eprintln!("replay: failed to read {}: {}", path, e);
        process::exit(1);
    });

    // v0.103: 解析失败以可读错误 + 退出码 2 报告（compile_and_opt 返回 Result）。
    let (func, witnesses) = match compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("replay: {}", e);
            process::exit(2);
        }
    };

    let type_errors = crate::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("replay: typeck failed, abort");
        process::exit(2);
    }

    let rec_path = recording_path(name);
    let mut interpreter = Interpreter::new();
    // v0.104.6 D178：先绑定再告警 —— 下面要把 recorder move 进 interpreter，
    // 移动之后就拿不到 `skipped_lines` 了。
    let rec = match record::Recorder::new_replay(rec_path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    interpreter.infra_mut().replace_recorder(rec);
    let mut env = interpreter.take_env();

    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    if let Err(e) = crate::mir::vm::run_mir(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut crate::mir::effect::Effects::new(),
    ) {
        eprintln!("Runtime error during replay: {}", e);
        process::exit(1);
    }
    if let Err(e) = crate::mir::vm::run_main_task(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut crate::mir::effect::Effects::new(),
    ) {
        eprintln!("Runtime error during replay main: {}", e);
        process::exit(1);
    }
    // v0.104.6 D182：报**实际重放了几次**，而不是「加载了几条事件」。
    //
    // 修前：`✓ replayed {} events`，那个数是 `recorder().events().len()`
    // —— **加载**的条数。一次都没命中时照样打 ✓。而其中 `state_mutation`
    // 之类**根本不可重放**（不是 `event_to_replay_entry` 的条目），
    // 那个数字天生虚高。实测：一次都没命中仍报
    // `✓ replayed 3 events`（D174 同族的假绿）。
    // v0.104.6 D182：报**实际重放了几次**，而不是「加载了几条事件」。
    let rec_ref = interpreter.infra().recorder();
    let loaded = rec_ref.events().len();
    let hits = rec_ref.replay_hits;
    let misses = rec_ref.replay_misses;
    // 录像里**可能**被重放的条目数（ai.chat / web.fetch）。
    let replayable = rec_ref
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e,
                record::Event::AiChat { .. } | record::Event::WebFetch { .. }
            )
        })
        .count();

    println!(
        "{} replayed {}/{} recorded call(s) from {}{}",
        // v0.104.6 D182：**0 命中时不给 ✓**。修前无论命中与否都打
        // `✓ replayed N events` —— 那个 ✓ 是「命令跑完了」的意思，
        // 却被读成「重放成功了」。两件事必须分开说。
        if hits > 0 { "✓" } else { "⚠" },
        hits,
        replayable,
        rec_path.display(),
        if loaded == replayable {
            String::new()
        } else {
            format!(
                " ({} event(s) loaded, {} of which replayable)",
                loaded, replayable
            )
        }
    );
    if misses > 0 {
        eprintln!(
            "[warn] {} recorded call lookup(s) did NOT match — those calls fell back to \
             mock / live API. Check that the prompts, model, and ai.chat signature are \
             unchanged since recording.",
            misses
        );
    }
    // v0.104.6 D182：**0 命中但录像里确有可重放条目** ⇒ 这次什么都没复现。
    // 与 D178 的 `audit` 同一原则：出结论型命令不能在没复现任何东西时
    // 让人以为重放成功了。此处只**大声说**（不擅自改退出码 ——
    // 那属 CLI 契约决定），但必须说。
    if hits == 0 && replayable > 0 {
        eprintln!(
            "✗ NOTHING was replayed: the recording has {} replayable call(s) but none matched.",
            replayable
        );
        eprintln!(
            "  If OPENAI_API_KEY is set, those calls may have hit the LIVE API — \
             replay is supposed to avoid exactly that."
        );
    }
    for w in &rec_ref.warnings {
        eprintln!("[warn] {}", w);
    }
}

pub fn run_diff(name_a: &str, name_b: &str) {
    let rec_a = recording_path(name_a);
    let rec_b = recording_path(name_b);

    // v0.104.6 D178：先保留 `Recorder` 再取 `events()`，以便把「跳过几行」
    // 报出来 —— 以前是 `r.events().to_vec()` 直接把 recorder 丢掉。
    let loaded_a = match record::Recorder::new_replay(rec_a.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("diff: {}: {}", rec_a.display(), e);
            process::exit(1);
        }
    };
    warn_skipped(&loaded_a);
    let events_a = loaded_a.events().to_vec();
    let loaded_b = match record::Recorder::new_replay(rec_b.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("diff: {}: {}", rec_b.display(), e);
            process::exit(1);
        }
    };
    warn_skipped(&loaded_b);
    let events_b = loaded_b.events().to_vec();

    let diff = record::diff_recordings(&events_a, &events_b);
    println!(
        "diff {} ({} events)  vs  {} ({} events):",
        name_a,
        events_a.len(),
        name_b,
        events_b.len()
    );
    println!();
    for line in &diff {
        println!("{}", line.render());
    }
    let identical = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::Identical(_, _)))
        .count();
    let changed = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::Changed(_, _, _)))
        .count();
    let only_a = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::OnlyInA(_, _)))
        .count();
    let only_b = diff
        .iter()
        .filter(|l| matches!(l, record::DiffLine::OnlyInB(_, _)))
        .count();
    println!();
    println!(
        "summary: identical={} changed={} only_in_{}={} only_in_{}={}",
        identical, changed, name_a, only_a, name_b, only_b
    );
}

pub fn run_record_list() {
    let dir = recordings_dir();
    match record::list_recordings(&dir) {
        Ok(infos) => {
            if infos.is_empty() {
                println!("No recordings found in {}", dir.display());
                return;
            }
            println!("Recordings ({}):\n", infos.len());
            println!(
                "{:<20} {:>8} {:>6} {:>20}",
                "NAME", "SIZE", "EVENTS", "LAST MODIFIED"
            );
            println!("{}", "-".repeat(60));
            for info in &infos {
                let size = format_size(info.size_bytes);
                // v0.104.6 D224：表头是 LAST MODIFIED，就该打**文件 mtime**
                let time = format_ts(info.modified_ms);
                println!(
                    "{:<20} {:>8} {:>6} {:>20}",
                    info.name, size, info.event_count, time
                );
            }
        }
        Err(e) => {
            eprintln!("record list: {}", e);
            process::exit(1);
        }
    }
}

pub fn run_record_stats(name: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record stats: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    let stats = record::compute_stats(rec.events());
    println!("Recording: {}", name);
    println!("{}", "-".repeat(40));
    println!("Events:        {} total", stats.total_events);
    println!("  ai.chat:     {}", stats.ai_chat_count);
    println!("  web.fetch:   {}", stats.web_fetch_count);
    println!("  notes:       {}", stats.note_count);
    // v0.104.6 D225：补齐其余两类，使子类之和**恒等于** `Events: … total`。
    // 修前 18 条全是 state_mutation 的录制会显示 0 + 0 + 0 却没有任何提示。
    println!("  messages:    {}", stats.msg_count);
    println!("  state mut:   {}", stats.state_mutation_count);
    println!("Errors:        {}", stats.error_count);
    println!("{}", "-".repeat(40));
    println!(
        "Tokens:        {} in + {} out = {} total",
        stats.total_tokens_in,
        stats.total_tokens_out,
        stats.total_tokens_in + stats.total_tokens_out
    );
    if let Some(avg_in) = stats.total_tokens_in.checked_div(stats.ai_chat_count) {
        let avg_out = stats.total_tokens_out / stats.ai_chat_count;
        println!("  avg/call:    {} in + {} out", avg_in, avg_out);
    }
    println!("{}", "-".repeat(40));
    println!("Latency:       {}ms total", stats.total_latency_ms);
    if stats.ai_chat_count + stats.web_fetch_count > 0 {
        let count = stats.ai_chat_count + stats.web_fetch_count;
        println!(
            "  avg:         {}ms",
            stats.total_latency_ms / count as u128
        );
        println!("  min:         {}ms", stats.min_latency_ms);
        println!("  max:         {}ms", stats.max_latency_ms);
    }
    println!("Duration:      {}", format_duration(stats.duration_ms));
    if !stats.models.is_empty() {
        println!("Models:        {}", stats.models.join(", "));
    }
}

pub fn run_record_export(name: &str, format: &str, output: Option<&str>) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record export: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    let fmt = match format {
        "md" | "markdown" => record::ExportFormat::Markdown,
        _ => record::ExportFormat::Jsonl,
    };
    let content = record::export_recording(rec.events(), &fmt, name);
    match output {
        Some(out_path) => {
            if let Err(e) = fs::write(out_path, &content) {
                eprintln!("record export: failed to write {}: {}", out_path, e);
                process::exit(1);
            }
            println!("✓ exported {} events -> {}", rec.events().len(), out_path);
        }
        None => print!("{}", content),
    }
}

/// v0.104.6: 0 事件快照的告警。
///
/// 装了录制器之后,`mora snapshot` 对**任何**输入都能失败 —— 除了
/// 一个残留面:脚本若一条可录事件都没产生(无 `ai.chat` / `web.fetch`
/// / 状态变更,例如整个文件只有 `print("hi")`),基线就是 0 条,
/// 0 vs 0 恒 Match → 依然恒绿,只是绿得「合法」了。
///
/// 那种快照**判别力为零**:它对任何改动都通过。这里明确说出来,
/// 而不是让 `✓ passed (0 events match)` 看起来像个有效的回归测试。
/// 退出码不变(0 事件是否该判失败属于语义裁决,不在此处擅自定)。
fn warn_if_no_events(summaries: &[record::EventSummary], name: &str, saving: bool) {
    if !summaries.is_empty() {
        return;
    }
    let verb = if saving { "saved" } else { "matched" };
    eprintln!(
        "[warn] snapshot '{}' has 0 recorded events — it can never fail, so '{}' proves nothing.",
        name, verb
    );
}

pub fn run_snapshot(
    file: &str,
    name: &str,
    update: bool,
    opt_level: Option<crate::mir::ssa::OptLevel>,
) {
    // v0.104.6 D189：别把一个「看起来是选项」的参数当路径去读。
    super::reject_option_as_path(file);
    let source = super::read_source(std::path::Path::new(file)).unwrap_or_else(|e| {
        eprintln!("snapshot: failed to read {}: {}", file, e);
        process::exit(1);
    });
    // v0.103: 解析失败以可读错误 + 退出码 2 报告（compile_and_opt 返回 Result）。
    let (func, witnesses) = match compile_and_opt(&source, opt_level) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snapshot: {}", e);
            process::exit(2);
        }
    };
    let type_errors = crate::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !type_errors.is_empty() {
        for err in &type_errors {
            eprintln!("{}", format_error(err));
        }
        eprintln!("snapshot: typeck failed");
        process::exit(2);
    }

    let mut interpreter = Interpreter::new();
    // v0.104.6: **装内存录制器** —— 这一行缺失是 `mora snapshot` 永久假绿的根因。
    // 不装的话 recorder 是默认 `new_off()`,`record_ai_chat` 的
    // `if !self.mode.is_record() { return; }` 挡掉一切, `current_events` 恒空,
    // `diff_snapshot` 拿 0 vs 0 比 → 恒 Match, 换什么 prompt 都报 "✓ passed",
    // exit 0。**永远不会失败**的快照比对等于没有比对。
    interpreter
        .infra_mut()
        .replace_recorder(record::Recorder::new_record_memory());
    let mut env = interpreter.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    if let Err(e) = crate::mir::vm::run_mir(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut crate::mir::effect::Effects::new(),
    ) {
        eprintln!("snapshot: runtime error: {}", e);
        process::exit(1);
    }
    if let Err(e) = crate::mir::vm::run_main_task(
        &func_arc,
        &mut interpreter,
        &mut env,
        &mut crate::mir::effect::Effects::new(),
    ) {
        eprintln!("snapshot: runtime error: {}", e);
        process::exit(1);
    }
    let current_events = interpreter.infra().recorder().events().to_vec();
    let snap_file = snapshot_path(name);
    if update || !snap_file.exists() {
        // 创建/更新基线
        let snap = record::create_snapshot(name, &current_events);
        let content = record::snapshot_to_jsonl(&snap);
        let dir = snapshots_dir();
        if !dir.exists()
            && let Err(e) = fs::create_dir_all(&dir)
        {
            eprintln!(
                "[warn] snapshot: failed to create dir {}: {}",
                dir.display(),
                e
            );
        }
        if let Err(e) = fs::write(&snap_file, &content) {
            eprintln!("snapshot: failed to write {}: {}", snap_file.display(), e);
            process::exit(1);
        }
        println!(
            "✓ snapshot '{}' saved ({} events)",
            name,
            snap.event_summaries.len()
        );
        warn_if_no_events(&snap.event_summaries, name, true);
    } else {
        // 对比基线
        let baseline_content = fs::read_to_string(&snap_file).unwrap_or_default();
        let baseline = match record::snapshot_from_jsonl(&baseline_content) {
            Some(b) => b,
            None => {
                eprintln!("snapshot: failed to parse baseline {}", snap_file.display());
                process::exit(1);
            }
        };
        let diffs = record::diff_snapshot(&baseline, &current_events);
        let mismatches: Vec<_> = diffs
            .iter()
            .filter(|d| !matches!(d, record::SnapshotDiff::Match(_)))
            .collect();
        if mismatches.is_empty() {
            println!(
                "✓ snapshot '{}' passed ({} events match)",
                name,
                baseline.event_summaries.len()
            );
            warn_if_no_events(&baseline.event_summaries, name, false);
        } else {
            eprintln!(
                "✗ snapshot '{}' FAILED ({} difference(s)):\n",
                name,
                mismatches.len()
            );
            for diff in &mismatches {
                match diff {
                    record::SnapshotDiff::CountMismatch { expected, actual } => {
                        eprintln!("  event count: expected={}, actual={}", expected, actual);
                    }
                    record::SnapshotDiff::EventChanged {
                        index,
                        expected,
                        actual,
                    } => {
                        eprintln!(
                            "  #{}: expected {:?} key={}",
                            index + 1,
                            expected.kind,
                            expected.key
                        );
                        eprintln!("       got      {:?} key={}", actual.kind, actual.key);
                    }
                    record::SnapshotDiff::EventAdded { index, actual } => {
                        eprintln!(
                            "  #{}: added {:?} key={}",
                            index + 1,
                            actual.kind,
                            actual.key
                        );
                    }
                    record::SnapshotDiff::EventMissing { index, expected } => {
                        eprintln!(
                            "  #{}: missing {:?} key={}",
                            index + 1,
                            expected.kind,
                            expected.key
                        );
                    }
                    _ => {}
                }
            }
            eprintln!("\nRun with --update to regenerate baseline");
            process::exit(1);
        }
    }
}

pub fn run_record_report(
    name: &str,
    note: Option<&str>,
    verify: Option<&str>,
    output: Option<&str>,
) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record report: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    let content = record::generate_report(rec.events(), name, note, verify, &[]);
    match output {
        Some(out_path) => {
            if let Err(e) = fs::write(out_path, &content) {
                eprintln!("record report: failed to write {}: {}", out_path, e);
                process::exit(1);
            }
            println!("✓ report generated -> {}", out_path);
        }
        None => print!("{}", content),
    }
}

pub fn run_record_audit(name: &str, policy_path: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record audit: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    // 加载 .moraignore 策略
    let ignore_rules = if Path::new(policy_path).exists() {
        let content = fs::read_to_string(policy_path).unwrap_or_default();
        record::parse_moraignore(&content)
    } else {
        Vec::new()
    };
    let findings = record::audit_recording(rec.events(), &ignore_rules);

    // v0.104.6 D178：**这是唯一一处「跳过行」必须**硬失败**的消费者。**
    //
    // 其余命令（stats / diff / export / timeline / report）是**汇报型**的 ——
    // 它们把事件数一并打出来，告警足够。
    //
    // 而 `audit` 回答的是一个**安全问题**：「这份录像能不能分享/提交」。
    // 有几行没能解析，就等于**有几行从未被检查过** —— 在那种数据上宣布
    // 「✓ No secrets found」是**假阴性**：扫描器给了它没有资格给的保证。
    // 修前正是这样（实测：3 行坏 1 行 → 「No secrets found」+ exit 0）。
    //
    // 故：数据不完整 ⇒ 无法出具结论 ⇒ 判失败，并说清缺了多少。
    if !rec.skipped_lines.is_empty() {
        eprintln!(
            "✗ audit INCONCLUSIVE for '{}': {} of {} line(s) could not be parsed, \
             so they were never scanned for secrets.",
            name,
            rec.skipped_lines.len(),
            rec.events().len() + rec.skipped_lines.len()
        );
        for s in rec.skipped_lines.iter().take(5) {
            eprintln!("    line {}: {}", s.line_no, s.excerpt);
        }
        if rec.skipped_lines.len() > 5 {
            eprintln!("    … and {} more", rec.skipped_lines.len() - 5);
        }
        eprintln!("  (a truncated recording is a common cause — re-record, or repair the file)");
        process::exit(1);
    }

    // v0.104.6 D180：规则披露**两个分支都打**。原先它只出现在
    // 「没发现密钥」那一支 —— 恰恰是**有发现**时你最想知道
    // 「我的 .moraignore 到底加载了没」的时候，它一句话不说。
    if !ignore_rules.is_empty() {
        let bad = record::unsupported_ignore_rules(&ignore_rules);
        println!(
            "  ({} rule(s) from {} loaded; {} usable)",
            ignore_rules.len(),
            policy_path,
            ignore_rules.len() - bad.len()
        );
        // 写了但不会被用上的规则必须点名 —— 一个不生效的策略文件
        // **比没有策略文件更糟**：用户以为限定了范围，实际什么都没发生。
        for line in &bad {
            println!("      [ignored] {}", line);
        }
    }

    if findings.is_empty() {
        println!("✓ No secrets found in recording '{}'", name);
    } else {
        println!(
            "⚠ {} potential secret(s) found in '{}':\n",
            findings.len(),
            name
        );
        println!("{:<6} {:<20} {:<20} PREVIEW", "EVENT", "FIELD", "PATTERN");
        println!("{}", "-".repeat(70));
        for f in &findings {
            println!(
                "{:<6} {:<20} {:<20} {}",
                f.event_id, f.field, f.pattern, f.preview
            );
        }
        println!(
            "\nRun with --policy {} to ignore known-safe patterns",
            policy_path
        );
        process::exit(1);
    }
}

pub fn run_record_timeline(name: &str) {
    let path = recording_path(name);
    let rec = match record::Recorder::new_replay(path.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("record timeline: {}", e);
            process::exit(1);
        }
    };
    warn_skipped(&rec);
    let rows = record::build_timeline(rec.events());
    if rows.is_empty() {
        println!("No events in recording {}", name);
        return;
    }
    println!("Timeline: {} ({} events)\n", name, rows.len());
    println!(
        "{:<4} {:<10} {:<50} {:>10} {:>8} {:>8}",
        "#", "KIND", "DETAIL", "TOKENS", "LAT(ms)", "STATUS"
    );
    println!("{}", "-".repeat(94));
    for row in &rows {
        println!(
            "{:<4} {:<10} {:<50} {:>10} {:>8} {:>8}",
            row.seq,
            row.kind,
            truncate(&row.detail, 50),
            row.tokens,
            row.latency_ms,
            row.status
        );
    }
}

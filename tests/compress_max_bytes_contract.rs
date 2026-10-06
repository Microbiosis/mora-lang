//! D227 取证普查：5 个内置子压缩器对 `SubCompressor::compress` 明文契约
//! 「压缩到不超过 max_bytes (UTF-8 字节)」的遵守情况。
//!
//! 判据形态是**网格普查**而非抽查：每个子压缩器 × 一组 max_bytes，
//! 断言 `out.len() <= max(max_bytes, content.len())`（不要求缩到比原文更小，
//! 但绝不允许超过用户设定的上限）。
//!
//! 这条判据在**修复前必须变红**。它不是新增期望，而是把 trait 签名上方
//! 注释里已写明的契约变成可执行断言。

use mora::compress::json::JsonSubCompressor;
use mora::compress::{CompressOptions, ContentRouter, SubCompressor};
use mora::compress::{code::CodeSubCompressor, html::HtmlSubCompressor};
use mora::compress::{log::LogSubCompressor, text::TextSubCompressor};

/// 一段能被 5 个子压缩器分别处理的样本语料。
fn corpus() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();

    // 1. 纯文本（6500 字节）
    let unit = "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
    out.push(("plain-text", unit.repeat(100)));

    // 2. 日志：60 行 × ~560 字节（远超 log.rs 硬编码的 80 字节/行假设）
    let head = "2026-07-01 10:00:00 INFO routine message ";
    let filler = "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ ".repeat(6);
    out.push(("log", format!("{}{}\n", head, filler).repeat(60)));

    // 3. 代码：签名行 + 长 body 行
    let mut code = String::new();
    for i in 0..40 {
        code.push_str(&format!("fn helper_{}() {{\n", i));
        code.push_str(
            &"    let value = compute(alpha, beta, gamma, delta, epsilon, zeta);\n".repeat(4),
        );
        code.push_str("}\n");
    }
    out.push(("code", code));

    // 4. HTML：大量文本节点
    let mut html = String::from("<html><body>");
    for i in 0..40 {
        html.push_str(&format!(
            "<p>paragraph {} with a good deal of filler text here</p>\n",
            i
        ));
    }
    html.push_str("</body></html>");
    out.push(("html", html));

    // 5. JSON 数组
    let mut json = String::from("[");
    for i in 0..40 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"id\":{},\"score\":{:.3},\"msg\":\"a message with filler text number {}\"}}",
            i,
            (i as f64) / 40.0,
            i
        ));
    }
    json.push(']');
    out.push(("json", json));

    out
}

/// 单个 (子压缩器, 语料) 对在给定 max_bytes 网格下是否全部遵守契约。
///
/// ⚠ 只在**该子压缩器 sniff 认可这段语料**时才判契约。
///
/// 这一点是判据自身修出来的第一版 bug：普查把「纯文本语料喂给
/// JsonSubCompressor」也算成违约，于是 48 条 `expected JSON array` 报错
/// 被计成「契约违反 48 处」。但 router 走的是 `sniff() >= 0.6` 筛选，
/// 文本语料下 json 的 sniff = 0.0，**永远不会**被路由到 json。
/// 拿不可能发生的调用去判契约，等于把判据的噪声当产品缺陷。
///
/// 修法：**sniff < 0.6 直接跳过该 (语料, 子压缩器) 组合**，
/// 只普查 router 真实会选的路径。断言里同时要求 `checked > 0`，
/// 防止「全跳过」也变绿。
fn violations_for(
    c: &dyn SubCompressor,
    content: &str,
    opts: &CompressOptions,
) -> Vec<(usize, usize, String)> {
    let mut bad = Vec::new();
    for mb in [16usize, 64, 200, 800, 2000, 5000] {
        match c.compress(content, mb, opts) {
            Ok(out) => {
                // 契约：输出不得超过 max_bytes
                if out.len() > mb {
                    bad.push((
                        mb,
                        out.len(),
                        format!("exceeds max_bytes by {}", out.len() - mb),
                    ));
                }
            }
            Err(e) => bad.push((mb, 0, format!("error: {e}"))),
        }
    }
    bad
}

#[test]
fn d227_every_subcompressor_respects_max_bytes() {
    let mut report = String::new();
    let mut total_violations = 0usize;
    let mut checked_pairs = 0usize;

    for (name, content) in corpus() {
        let pairs: Vec<(&str, Box<dyn SubCompressor>)> = vec![
            ("text", Box::new(TextSubCompressor)),
            ("log", Box::new(LogSubCompressor)),
            ("code", Box::new(CodeSubCompressor)),
            ("html", Box::new(HtmlSubCompressor)),
            ("json", Box::new(JsonSubCompressor)),
        ];
        for (sc_name, sc) in pairs {
            // 与 ContentRouter::sniff 同一门槛：< 0.6 的组合 router 不会选
            if sc.sniff(&content) < 0.6 {
                continue;
            }
            checked_pairs += 1;
            for strategy in ["auto", "head_tail"] {
                let opts = CompressOptions {
                    strategy: strategy.to_string(),
                    ..Default::default()
                };
                let bad = violations_for(sc.as_ref(), &content, &opts);
                if !bad.is_empty() {
                    total_violations += bad.len();
                    for (mb, len, why) in &bad {
                        report.push_str(&format!(
                            "  {name}/{sc_name}[{strategy}] max_bytes={mb} -> len={len}  ({why})\n"
                        ));
                    }
                }
            }
        }
    }

    assert!(
        checked_pairs > 0,
        "grid must actually exercise compressors (sniff filter removed everything)"
    );
    assert_eq!(
        total_violations, 0,
        "SubCompressor::compress 契约「压缩到不超过 max_bytes」被违反 {} 处:\n{report}",
        total_violations
    );
}

#[test]
fn d227_auto_strategy_respects_max_bytes_end_to_end() {
    // 端到端：走 ContentRouter（与 compress_top 的 "auto" 分支同一路径）
    let router = ContentRouter::default_router();
    let mut checked = 0usize;

    for (name, content) in corpus() {
        // v0.104.6 D228：此处修前 panic —— `sniff` 返回 None，
        // 因为 text 的兜底分 0.5 永远过不了 `>= 0.6` 的门槛。
        let comp = router
            .sniff(&content)
            .unwrap_or_else(|| panic!("{name}: no compressor matched, cannot test auto path"));
        for mb in [16usize, 64, 200, 800, 2000, 5000] {
            let opts = CompressOptions::default();
            let out = comp
                .compress(&content, mb, &opts)
                .unwrap_or_else(|e| panic!("{name}/{mb}: {e}"));
            checked += 1;
            assert!(
                out.len() <= mb,
                "auto 路由到 {} 但违反契约: corpus={name} max_bytes={mb} len={} original={}\n\
                 契约（SubCompressor::compress 文档）：「压缩到不超过 max_bytes (UTF-8 字节)」",
                comp.origin(),
                out.len(),
                content.len()
            );
        }
    }
    assert!(checked > 0, "grid must actually exercise compressors");
}

#[test]
fn d227_head_pct_plus_tail_pct_over_one_does_not_duplicate_content() {
    // head_pct + tail_pct > 1 时 head 段与 tail 段**重叠**，
    // 重叠部分被输出两次 → 结果比原文更长。
    let unit = "0123456789 ABCDEFGHIJKLMNOP\n";
    let content = unit.repeat(20);
    let opts = CompressOptions {
        strategy: "head_tail".into(),
        head_pct: 0.6,
        tail_pct: 0.6,
        ..Default::default()
    };
    let out = TextSubCompressor
        .compress(&content, 64, &opts)
        .expect("compress should not error");

    assert!(
        out.len() <= content.len(),
        "head_pct=0.6 + tail_pct=0.6 (和 > 1) 时输出 {} 字节 > 原文 {} 字节 —— \
         head 段与 tail 段重叠，同一内容被重复输出",
        out.len(),
        content.len()
    );
}

#[test]
fn d227_head_tail_never_grows_input_at_all() {
    // 最强形式：head_tail 的输出既不超过 max_bytes，也绝不超过原文长度
    let unit = "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
    let content = unit.repeat(100);
    for (hp, tp) in [(0.15, 0.15), (0.3, 0.3), (0.6, 0.6), (0.9, 0.9), (1.0, 1.0)] {
        for mb in [16usize, 64, 200, 800, 2000] {
            let opts = CompressOptions {
                strategy: "head_tail".into(),
                head_pct: hp,
                tail_pct: tp,
                ..Default::default()
            };
            let out = TextSubCompressor
                .compress(&content, mb, &opts)
                .expect("compress should not error");
            assert!(
                out.len() <= mb.min(content.len()),
                "head_pct={hp} tail_pct={tp} max_bytes={mb}: len={} 超过 min(max_bytes, original)={}",
                out.len(),
                mb.min(content.len())
            );
        }
    }
}

/// v0.104.6 D227：`JsonSubCompressor` 必须真的按预算收缩条目数。
///
/// 修前是 `target = max_bytes / 200` —— **每项 200 字节**的无根据假设。
/// 实测每项 ~68 字节时，`max_bytes=16` 算出 target=1，但输出仍是 148 字节
/// （超限 8.25 倍）。修后改为「序列化结果装不下就把 target 减半」循环收缩。
///
/// 判据写成**单调性不变式**而不是具体条目数：预算越大，输出越长，
/// 且始终 ≤ max_bytes。回退到「不减半」时，收缩循环失效，
/// 小预算下的输出会超出上限。
#[test]
fn d227_json_compressor_shrinks_items_to_fit_budget() {
    let c = JsonSubCompressor;
    let opts = CompressOptions::default();

    // 每项约 68 字节 —— 远小于 200，让「每项 200 字节」的假设算错
    let json = format!(
        "[{}]",
        (0..40)
            .map(|i| format!("{{\"id\":{i},\"msg\":\"a message with filler text number {i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(json.len() > 1000, "语料必须足够长才有意义");

    let mut prev_len = 0usize;
    for mb in [64usize, 128, 256, 512, 1024] {
        let out = c
            .compress(&json, mb, &opts)
            .unwrap_or_else(|e| panic!("json compress must not error: {e}"));
        assert!(
            out.len() <= mb,
            "D227: max_bytes={mb} 时输出 {} 字节超限（逐项 200 字节的假设失效）",
            out.len()
        );
        assert!(
            out.len() >= prev_len,
            "D227: 预算从 {} 增到 {mb}，输出不应变短（{} → {}）",
            if mb == 64 { 0 } else { mb / 2 },
            prev_len,
            out.len()
        );
        prev_len = out.len();
    }
}

/// v0.104.6 D227：`CodeSubCompressor` / `HtmlSubCompressor` 的预算检查
/// 必须在**追加之前**，且要为尾部 marker 留空间。
///
/// 修前是 `if out.len() >= max_bytes { break; }` —— 在追加完本行之后才判断，
/// 随后还要追加 marker，输出必然超限（实测 max_bytes=16 → 79 字节）。
#[test]
fn d227_code_and_html_reserve_room_for_trailing_marker() {
    let code = "fn main() {\n    let x = 1;\n}\n".repeat(40);
    let html = "<html><body>\n<p>a paragraph of filler text here</p>\n</body></html>".repeat(30);

    for (name, sc) in [
        (
            "code",
            Box::new(CodeSubCompressor) as Box<dyn SubCompressor>,
        ),
        (
            "html",
            Box::new(HtmlSubCompressor) as Box<dyn SubCompressor>,
        ),
    ] {
        let content = if name == "code" { &code } else { &html };
        let opts = CompressOptions::default();
        for mb in [16usize, 32, 64, 128, 256, 1024] {
            let out = sc
                .compress(content, mb, &opts)
                .unwrap_or_else(|e| panic!("{name}/{mb}: {e}"));
            assert!(
                out.len() <= mb,
                "D227: {name} max_bytes={mb} 时输出 {} 字节超限 \
                 （预算检查在追加之后，且没给尾部 marker 留空间）",
                out.len()
            );
        }
    }
}
/// v0.104.6 D227：**正文**不得比原文更长（marker 豁免）。
///
/// 这条与上一条互补：上一条管「不超 max_bytes」，本条管「不放大」。
///
/// ⚠ marker 豁免不是随意放宽，而是本轮**实测逼出来的**：判据最初写成
/// 「输出不得比原文长」，结果 1241 字节的日志（body 1241 + marker 91）
/// 因 `1241 + 91 > 1241` 整体退回原文，`ERROR lines preserved` marker
/// 凭空消失。marker 是元信息（保留了哪些错误行），为它丢掉全部元信息
/// 得不偿失。故「不放大」只约束正文。
///
/// 判据形态：从输出里剥掉尾部的 `<compressed:method=…>` 段后，
/// 剩余正文不得比原文长。
#[test]
fn d227_compressed_body_never_exceeds_original() {
    for (name, content) in corpus() {
        let pairs: Vec<(&str, Box<dyn SubCompressor>)> = vec![
            ("text", Box::new(TextSubCompressor)),
            ("log", Box::new(LogSubCompressor)),
            ("code", Box::new(CodeSubCompressor)),
            ("html", Box::new(HtmlSubCompressor)),
            ("json", Box::new(JsonSubCompressor)),
        ];
        for (sc_name, sc) in pairs {
            if sc.sniff(&content) < 0.6 {
                continue;
            }
            for mb in [200usize, 2000, 5000] {
                let opts = CompressOptions::default();
                let out = sc
                    .compress(&content, mb, &opts)
                    .unwrap_or_else(|e| panic!("{name}/{sc_name}/{mb}: {e}"));
                // 剥掉尾部 marker：<compressed:method=...> 或 elided marker
                let body = out
                    .rsplit_once("<compressed:method=")
                    .map(|(b, _)| b)
                    .unwrap_or_else(|| {
                        out.rsplit_once("bytes elided")
                            .map(|(b, _)| b)
                            .unwrap_or(&out)
                    });
                assert!(
                    body.len() <= content.len(),
                    "{name}/{sc_name} max_bytes={mb}: 正文 {} 字节 > 原文 {} 字节 —— \
                     压缩器放大了内容",
                    body.len(),
                    content.len()
                );
            }
        }
    }
}

// ──────────────────── D228：`auto` 的兜底压缩器必须真的兜底 ────────────────────

/// v0.104.6 D228：`ContentRouter::sniff` 的门槛 `>= 0.6` 把兜底者
/// （`TextSubCompressor::sniff` 固定 0.5）永久排除，于是
/// `compress(纯文本, "auto")` 报 `no compressor matched` 并 exit 1。
///
/// 判据分两部分，缺一不可：
/// 1. **兜底存在** —— 任何内容都必须路由到某个子压缩器。
/// 2. **选优仍生效** —— 门槛降低后不能退化成「永远选 text」：
///    json / html 语料必须仍路由到各自的专用压缩器。
///
/// 只写第 1 条会漏掉「降门槛导致 json 语料也走 text」的回归；
/// 只写第 2 条则完全测不到 D228。
#[test]
fn d228_auto_strategy_always_has_a_fallback_compressor() {
    let router = ContentRouter::default_router();

    // 1. 兜底：三类「专用压缩器都不认」的语料
    let plain = "the quick brown fox jumps over the lazy dog\n".repeat(50);
    let prose = "这是一段没有任何结构标记的中文散文，用来验证兜底路径。\n".repeat(50);

    for (name, content) in [("plain-ascii", &plain), ("chinese-prose", &prose)] {
        let comp = router.sniff(content);
        assert!(
            comp.is_some(),
            "D228: {name} 未匹配到任何子压缩器 —— 兜底压缩器失效，\
             compress(x, \"auto\") 会报 'no compressor matched' 并 exit 1"
        );
        assert_eq!(
            comp.expect("just checked").origin(),
            "text",
            "D228: {name} 应落到兜底的 text 压缩器"
        );
    }
}

#[test]
fn d228_lowering_threshold_does_not_break_specialised_routing() {
    let router = ContentRouter::default_router();

    // json 语料 → JsonSubCompressor（0.9，最高分）
    let json = format!(
        "[{}]",
        (0..10)
            .map(|i| format!("{{\"id\":{i},\"msg\":\"m{i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(
        router.sniff(&json).expect("json must route").origin(),
        "json",
        "D228: 降门槛后 JSON 语料不应被兜底的 text 抢走"
    );

    // html 语料 → HtmlSubCompressor（0.8）
    let html = "<div><p>a</p></div>\n".repeat(20);
    assert_eq!(
        router.sniff(&html).expect("html must route").origin(),
        "html",
        "D228: 降门槛后 HTML 语料不应被兜底的 text 抢走"
    );
}

/// v0.104.6 D227：head_tail 的 marker 必须报**实际**保留比例。
///
/// 修前报的是请求的 `head_pct` / `tail_pct` 原值。预算饱和时二者分叉：
/// `pct=0.5+0.5`、`max_bytes=1000`、`total=6500` 实测 marker 写
/// 「head_tail 50% + 50%」，而实况只留 975/6500 ≈ 15%。
///
/// 判据写成**不变式**而不是具体数字：从 marker 里解析出的两个百分比，
/// 乘以原文长度后必须落在实际保留的字节区间内（允许 1% 的取整误差），
/// 且三者之和（head% + tail% + elided%）≈ 100%。
#[test]
fn d227_head_tail_marker_reports_actual_not_requested_ratio() {
    let unit = "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
    let content = unit.repeat(100); // 6500 bytes
    let total = content.len() as f64;

    // 预算饱和：请求 50%+50%，但 max_bytes 只允许留 15%
    for (hp, tp, mb) in [(0.5, 0.5, 1000usize), (0.3, 0.3, 500), (0.9, 0.9, 2000)] {
        let opts = CompressOptions {
            strategy: "head_tail".into(),
            head_pct: hp,
            tail_pct: tp,
            ..Default::default()
        };
        let out = TextSubCompressor
            .compress(&content, mb, &opts)
            .expect("compress should not error");

        // 解析 marker: `... [N bytes elided (head_tail H% + T%)] ...`
        let start = out
            .find('[')
            .unwrap_or_else(|| panic!("no elided marker in output:\n{out}"));
        let rest = &out[start + 1..];
        let end = rest.find(']').expect("marker must be closed");
        let inside = &rest[..end];
        let mut parts = inside.split_whitespace();
        let elided: f64 = parts
            .next()
            .expect("elided count token")
            .parse()
            .unwrap_or_else(|_| panic!("elided count must be numeric, got {inside:?}"));
        let ratios = inside
            .split("(head_tail ")
            .nth(1)
            .unwrap_or_else(|| panic!("ratios missing in {inside:?}"));
        let fields: Vec<&str> = ratios.split('%').collect();
        let head_p: f64 = fields[0].trim().parse().expect("head pct numeric");
        // 第二个字段带分隔符：`+ 8`
        let tail_p: f64 = fields[1]
            .trim()
            .trim_start_matches('+')
            .trim()
            .parse()
            .expect("tail pct numeric");

        // 不变式 1：三个百分比加起来应当是 100%（elided% 用字节数算）
        let elided_p = elided * 100.0 / total;
        let sum = head_p + tail_p + elided_p;
        assert!(
            (sum - 100.0).abs() <= 2.0,
            "D227: marker 的 head%({head_p}) + tail%({tail_p}) + elided%({elided_p:.1}) \
             应约等于 100，实得 {sum:.1} —— marker 在自述一个不成立的比例分布 \
             (请求 {hp}/{tp}, max_bytes={mb})"
        );

        // 不变式 2：报出的 head% 不得虚高到接近请求值而实况远小于它
        let actual_head_bytes = head_p / 100.0 * total;
        let actual_tail_bytes = tail_p / 100.0 * total;
        assert!(
            actual_head_bytes + actual_tail_bytes <= out.len() as f64 + 2.0,
            "D227: marker 声称保留 {:.0} 字节（{head_p}% + {tail_p}%）但输出只有 {} 字节 \
             —— marker 报的是请求值而不是实际值 (请求 {hp}/{tp}, max_bytes={mb})",
            actual_head_bytes + actual_tail_bytes,
            out.len()
        );
    }
}

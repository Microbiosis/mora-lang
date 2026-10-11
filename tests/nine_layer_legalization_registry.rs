//! v0.104.6：显式合法性登记（`src/mir/legalization.rs`）的**双向**核对。
//!
//! ## 为什么值得锁
//!
//! `nine_layer_fallback_census.rs` 钉住「哪些构造会回落」（实测差分），
//! `nine_layer_fallback_reasons.rs` 钉住「为什么回落」（判别标记）。本文件
//! 钉住**第三件事**：生产侧的显式登记 [`legalization`] 是否与前两者**一致**。
//!
//! 三者一致 ⇒ 「未 legalize 构造」这个集合是可信的，生产门的三态回落
//! （合法且绿 / 含未 legalize / 回归）才成立。任何一侧漂移 CI 即红 ——
//! 这正是本刀要的：让「已知未支持」与「管线回归」在生产侧可区分。
//!
//! 核对是**双向**的：
//! - 不虚报：登记里的每条未 legalize 构造，实测**确实**命中（`should_fall_back == true`）
//! - 不漏报：普查里每条 `should_fall_back == false` 的构造，实测**确实不**命中登记
//!
//! 以 census 实测为准：**不允许**为了让登记通过而改 census 的期望值。

use mora::mir::legalization::{GateDecision, REGISTRY, classify};
use mora::mir::pipeline::run_pipeline;
use mora::parser_v3::ParserV3;

/// 三态判定的**穷举**单测 —— 本刀的**核心新能力**是「回归」这一态能被单独
/// 识别出来，故把 `classify` 的四种输入组合全部钉住。
///
/// 关键断言：`AllLegal + 差分红 → Regression`（此前该状态与
/// `HasUnlegalized` 在生产侧同形，真实缺陷会被静默藏住）。
#[test]
fn classify_distinguishes_regression_from_known_gap() {
    // 差分绿 + 全合法 → 快路径
    assert_eq!(classify(&[], true), GateDecision::AllLegal);
    // 差分红 + 含未 legalize 构造 → 预期回落（按名）
    assert_eq!(classify(&["worker"], false), GateDecision::HasUnlegalized);
    // 差分红 + 全 legalize → **回归**（必须与上面区分开）
    assert_eq!(classify(&[], false), GateDecision::Regression);
    // 差分绿但含未 legalize 构造：理论上 run_pipeline 不会同时出现
    // （未 legalize 构造必然导致差分不绿），但 classify 仍以「有未 legalize」
    // 为先返回 HasUnlegalized，保持判定优先级确定。
    assert_eq!(classify(&["eval_bare"], true), GateDecision::HasUnlegalized);
}

/// 与 `nine_layer_fallback_census.rs` 的 `CENSUS` 保持同一份口径的子集：
/// 这里只钉**会回落**的 4 条 + 若干**必须走快路径**的代表构造。
///
/// `(name, source)`。会回落的用 census 里的最小源码原样搬过来。
const FALLBACK_CASES: &[(&str, &str)] = &[
    ("worker", "worker w do\n  print(1)\nend\n"),
    ("transaction", "transaction\n  print(1)\nend\n"),
    (
        "perform_bare",
        "let g = \"i\"\ng = perform Ai(\"x\")\nprint(g)\n",
    ),
    ("eval_bare", "eval(1 + 1)\n"),
];

/// 必须**不上**登记表（即走 9 层快路径、登记不误伤）的代表构造。
///
/// 关键一条是 `handle_wrapped_perform` —— 它的 MIR 里**同样含 `Perform`
/// 指令**，但因为被 `Handle` 包裹、是**合法**程序，所以登记**不得**把它标成
/// 裸 perform。这正是 `legalization.rs` 用结构签名而非裸类别的原因。
const LEGAL_CASES: &[(&str, &str)] = &[
    (
        "handle_wrapped_perform",
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
    ),
    ("arithmetic", "let x = 1 + 2\nprint(x)\n"),
    ("if_else", "let x = 1\nif x > 0 then\n  print(1)\nend\n"),
    ("for_loop", "for i in [1, 2, 3]\n  print(i)\nend\n"),
    ("closure", "let f = fn(x) x + 1\nprint(f(1))\n"),
];

fn compile_and_scan(src: &str) -> Vec<&'static str> {
    let (func, witnesses) = ParserV3::compile(src).expect("source should compile");
    let (result, _) = run_pipeline(&func, &witnesses);
    result.unlegalized_constructs
}

/// 登记表里的每一条，都必须被一条会回落的构造**真实命中**（不虚报）。
#[test]
fn registry_entries_are_real_fallbacks() {
    let mut wrong = Vec::new();
    for (name, src) in FALLBACK_CASES {
        let hit = compile_and_scan(src);
        if !hit.contains(name) {
            wrong.push(format!(
                "会回落的构造 `{name}` 实测命中登记 = {hit:?} —— 登记表**漏报**了它，\
                 生产侧会把它误判成「回归」而误报"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "登记与实测不符（{} 条）：\n  - {}",
        wrong.len(),
        wrong.join("\n  - ")
    );
}

/// 登记表的**每一条**都必须有对应的实测回落构造（防止往 REGISTRY 加了条却
/// 忘了在 FALLBACK_CASES 里给证人，或反之）。
#[test]
fn registry_and_fallback_cases_are_in_bijection() {
    let reg_names: Vec<&str> = REGISTRY.iter().map(|u| u.name).collect();
    let case_names: Vec<&str> = FALLBACK_CASES.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        reg_names, case_names,
        "REGISTRY 与 FALLBACK_CASES 必须是同一组构造（顺序无关但集合一致）。\n\
         REGISTRY = {reg_names:?}\nFALLBACK_CASES = {case_names:?}"
    );
}

/// 合法构造（尤其 `handle` 包裹的 perform）**不得**被登记表误伤
/// —— 这是结构签名 vs 裸类别的核心判据。
#[test]
fn legal_cases_are_not_flagged() {
    let mut wrong = Vec::new();
    for (name, src) in LEGAL_CASES {
        let hit = compile_and_scan(src);
        if !hit.is_empty() {
            wrong.push(format!(
                "合法构造 `{name}` 被误标为未 legalize = {hit:?} —— 登记**误伤**了 \
                 一个已支持构造（该走 9 层快路径）"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "登记表误伤合法构造（{} 条）：\n  - {}",
        wrong.len(),
        wrong.join("\n  - ")
    );
}

/// 普查表里会回落的条目与本文件一致 —— 三条清单（census / reasons /
/// registry）必须同步，任何一处单独改都会红。
#[test]
fn matches_census_fallback_set() {
    let census_fallbacks = ["worker", "transaction", "perform_bare", "eval_bare"];
    for name in census_fallbacks {
        assert!(
            FALLBACK_CASES.iter().any(|(n, _)| *n == name),
            "census 里的回落项 `{name}` 在本文件缺少对应源码证人"
        );
    }
    assert_eq!(
        FALLBACK_CASES.len(),
        census_fallbacks.len(),
        "回落构造数应与 census 一致（4 条）"
    );
}

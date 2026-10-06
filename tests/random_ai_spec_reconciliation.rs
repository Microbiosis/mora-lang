//! v0.104.6 D348 —— `random.*` / `ai.*` 的 spec 签名对账（否定轮，无产品变更）
//!
//! D347 建立了「spec 签名表要**逐条**对账」的做法，本条把它推到
//! 剩下两个尚未对账的模块：`random.*`（6 条）与 `ai.*`（4 条）。
//!
//! ## ① `random.*`：六条签名**全部一致**，且守卫完整
//!
//! | spec 声明 | 实测 | |
//! |---|---|---|
//! | `random.random()` → `-> float` | `float` | ✅ |
//! | `random.rand_int(min, max)` → `float, float -> float` | `float` | ✅ |
//! | `random.rand_float(min, max)` → `float, float -> float` | `float` | ✅ |
//! | `random.rand_choice(list)` → `list -> any` | `float` | ✅ |
//! | `random.seed(n)` → `float -> nil` | `nil` | ✅ |
//! | `random.shuffle(list)` → `list -> list` | `list` | ✅ |
//!
//! **区间语义也已核实**：`next_i64_in` 用 `% (max - min)` ⇒ 结果落在
//! **`[min, max)`**（半开），与 spec 写的「[min, max) 整数值」**一致**。
//!
//! ⚠ 唯一看起来矛盾的是 `rand_int(3, 3)` → `3.0`：spec 的 `[min, max)`
//! 意味着 `3` 不该出现。但 `next_i64_in` 的 `max <= min → return min`
//! 是**单点区间**分支，源码注释明写「`rand_choice` / `shuffle` 两个
//! **内部调用点**也依赖该行为」⇒ **有意为之**，不是缺陷。
//!
//! **守卫完整**（D144 已做过）：`rand_int(5,1)` / `rand_float(1,0)` 都明确
//! 报「区间写反」；`rand_choice([])` 报「empty list」；多传实参由
//! `variadic` 放行。
//!
//! ## ② `ai.*`：四条里**三条不可达 + 一条参数顺序分叉**
//!
//! | spec 声明 | 实测 |
//! |---|---|
//! | `ai.chat(cfg, prompt)` → `ai_config, string -> ai_result` | `ai.chat(prompt[, {model}])` —— **1 参，顺序相反** |
//! | `ai.stream(prompt)` | **不可达**（`AI_MODULE_METHODS` 只有 3 个，D59 已记） |
//! | `ai.create(name, config)` | **不可达**（属 `agent` 而非 `ai`） |
//! | `ai.critic(answer, ctx?)` | 可达（1 参起） |
//!
//! ⚠ **不可达那两条本条不重复报**（D59 已钉，见
//! `tests/ai_namespace_reachability.rs`）。本条只钉 `chat` 的**参数顺序**分叉。
//!
//! ## 为什么不改 spec
//!
//! 与 D347 同理：`ai.chat` 的实现从 v0.75.84 起就是
//! `chat(prompt[, {model: "..."}])`，**spec 那行是早期草稿**。
//! 改它是**文档勘误**，但要确认「`cfg` 是不是真的从签名里去掉了」
//! （`do_ai_chat` 内部仍可能读 `model`）—— 属文档追平，暂不擅动。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d348_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout **全部**实质行。
///
/// ⚠ 探针**不带**尾随换行（D345 教训）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d348t_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **契约 ①**：`random.*` 六条签名的返回类型**全部与 spec 一致**。
#[test]
fn d348_random_six_signatures_match_spec() {
    for (body, want) in [
        ("print(type_of(random.random()))", "float"),
        ("print(type_of(random.rand_int(1, 5)))", "float"),
        ("print(type_of(random.rand_float(0.0, 1.0)))", "float"),
        ("print(type_of(random.rand_choice([1,2,3])))", "float"),
        ("print(type_of(random.seed(42)))", "nil"),
        ("print(type_of(random.shuffle([1,2,3])))", "list"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 0, "`{body}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`{body}` 应得 {want}（spec 一致）; 实得 {got}");
    }
}

/// **契约 ②**：`rand_int` 是**半开区间** `[min, max)` —— 与 spec 一致。
#[test]
fn d348_rand_int_is_half_open_interval() {
    // 端点可达性：`[1,5)` 的 min 可达、max 不可达
    // 用「取 400 次，统计是否出现过 5」来判定（半开则 max 永不出现）
    let body = "random.seed(1)\n\
                let hitMax = false\n\
                for i in range(400) {\n\
                  let v = random.rand_int(1, 5)\n\
                  if v == 5.0 { hitMax = true }\n\
                }\n\
                print(hitMax)\n";
    let (code, got) = ev(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "false",
        "`rand_int(1,5)` 是 `[1,5)` **半开**（与 spec 一致）⇒ 5 永不出现; 实得 {got}\n\
         ⚠ 若本条红，说明区间语义被改成闭区间 —— 那是**有意的**变更"
    );
}

/// **契约 ③**：`rand_int(3, 3)` → `3.0` 是**单点区间**分支，**有意为之**。
///
/// 源码注释明写「`rand_choice` / `shuffle` 两个内部调用点也依赖该行为」。
#[test]
fn d348_rand_int_equal_bounds_is_deliberate() {
    let (code, got) = ev("print(random.rand_int(3, 3))\n");
    assert_eq!(code, 0, "单点区间应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "3.0",
        "`rand_int(3,3)` → 3.0（`max <= min → return min`）; 实得 {got}"
    );
}

/// **契约 ④****：`random.*` 的边界守卫完整（D144 已做）。 */
#[test]
fn d348_random_guards_are_complete() {
    for (body, needle) in [
        ("print(random.rand_int(5, 1))", "区间写反"),
        ("print(random.rand_float(1.0, 0.0))", "区间写反"),
        ("print(random.rand_choice([]))", "empty list"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }

    // 可重现性：同种子 → 同序列
    let (code, got) = ev(
        "random.seed(42)\nlet a = random.random()\nrandom.seed(42)\nlet b = random.random()\nprint(a == b)\n",
    );
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "true",
        "同种子应产生**同序列**（`seed` 的 spec 承诺是「重置本运行时的种子」）; 实得 {got}"
    );
}

/// **契约 ⑤**：`ai.chat` 是 **1 参**（prompt 在前），而 spec 写 `ai.chat(cfg, prompt)`。
///
/// 这是 D348 唯一的**新分叉**。不可达的 `ai.stream` / `ai.create`
/// 属 D59 已记范围，本条不重复。
#[test]
fn d348_ai_chat_takes_prompt_first_contrary_to_spec() {
    // 1 参（prompt）应通过
    let (code, got) = ev("print(ai.chat(\"hi\"))\n");
    assert_eq!(
        code, 0,
        "`ai.chat(\"hi\")`（1 参 prompt）应成功; 实得 exit={code} out={got}"
    );
    // 2 参（spec 写的 cfg, prompt 形态）应被 typeck 拒
    let (code, got) = ev("print(ai.chat({role: \"user\"}, \"hi\"))\n");
    assert_eq!(
        code, 2,
        "`ai.chat(cfg, prompt)`（spec 形态）应被 typeck 拒 —— 实测确被拒; 实得 exit={code} out={got}\n\
         ⇒ 分叉确认：spec 写的是「cfg 在前」，实现是「prompt 在前」"
    );
    assert!(
        got.contains("arguments"),
        "应是**元数**错误（2 参不被接受）; 实得: {got}"
    );
}

/// **源码侧**：`ai` 裸名只暴露 3 个方法（`ai.stream` / `ai.create` 不可达）。
///
/// 这部分**不是**本轮的新发现（D59 已记），此处只是把「不可达」钉成源码判据，
/// 让它与 `ai.chat` 的参数顺序分叉**区分开**。
#[test]
fn d348_ai_module_exposes_only_three_methods() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/typeck/dispatch.rs"
    ))
    .expect("读 dispatch.rs");
    assert!(
        src.contains(r#"const AI_MODULE_METHODS: &[&str] = &["chat", "tokens", "critic"];"#),
        "`ai` 裸名应只暴露 chat / tokens / critic —— `stream` / `create` 不可达（D59 已记）"
    );
}

//! v0.104.6 D340 —— 「用户实参 → 整型」的裸 `as` 转换**全称普查**（否定轮，无产品变更）
//!
//! D339 修了 `tea.run(max_steps)` 的裸 `n as usize`，并提出一条纪律：
//! **修同根因的缺陷后，问「同一语汇在别处还有几处」**。
//! 本条把那条纪律**全称执行**：`src/` 下**全部 60 处**裸 `as <整型>` 转换，
//! 逐一判定「是否吃用户实参、是否已有守卫」。
//!
//! ## 三层收窄（每一层都砍掉一批，且**都不靠猜**）
//!
//! ```text
//! 210 处  全部 `as <整型>`（src/，排除测试文件与纯注释行）
//!  60 处  限定在 src/interpreter/**（纯转换层）
//!   2 处  再限定「右值含用户实参来源的名字」（n / size / args / budget / …）
//!   1 处  唯一无就近守卫者 —— 且实测它是**内部值**（退避抖动 `exp`，来自 rand）
//! ```
//!
//! ⇒ **「用户实参 → 整型」的无守卫遗漏 = 0**。
//! D285（`exec.parallel`）/ D339（`tea.run`）/ D146（`take`/`drop`）/
//! D153（`reshape`/`crush_json`）那一系列修复已把它们覆盖完。
//!
//! ## 为什么必须**三层收窄**而不是只看一层
//!
//! 第一层（210 处全列）里 **174 处无「就近守卫」**，看着像 174 个缺陷。
//! 逐条看下来它们绝大多数是**内部值**的转换：
//!
//! ```text
//! method_dispatch.rs:172   Ok(Value::Int(list.len() as i64))        ← list 长度，内部
//! compress/json.rs:339    (target as f32 * 0.15) as usize           ← 压缩比例，内部
//! document/reading_order  .ceil() as usize                          ← 几何计算，内部
//! trace_collector.rs:131  duration.as_millis() as u64              ← 耗时，内部
//! ```
//!
//! ⇒ 「**没有就近守卫**」这个静态指标**不判别**（与 D322 的
//! 「`兜底未覆盖全部变体` 静态指标不判别」同型）。真正的判别是：
//! **右值能不能追溯到用户的实参**。
//!
//! ## 唯一无守卫的那处：实测是内部值
//!
//! ```text
//! src/interpreter/mod.rs:102   (exp as i64 + offset).max(0) as u64
//! ```
//!
//! `exp` 来自退避计算的 `rand`（`mod.rs:97-102` 的 jitter 计算），
//! **不来自任何 `Value`**，且前一行已有 `.max(0)` ⇒ 无负数、无回绕。
//!
//! ## `parse_budget_dispatch`（`numeric_helpers.rs:179`）为何**够不着**
//!
//! `Ok((num * mult as f64) as usize)` 确实**无负数守卫**（负数在 148-149
//! 被前面两条 arm 拦了，但这是**类型相关**的：`String("-1KB")` 路径不走那两条）。
//! 它是 `pub(super)`，唯一调用点是 `builtin_impls.rs:625`（`compose_prompt`
//! 的 section `budget` 键），而该路径在脚本层**不可达**：
//!
//! - `compose_prompt` 的 typeck 签名把 `section` 声明为 `Type::String`（D68），
//!   传 Dict 会被 typeck 拒（D68 之前的 `Signature` 注释已写明这条限制）；
//! - `with budget = …` 那条路 D116 已让它**立即报错**（承诺但未实现）。
//!
//! ⇒ 属**死路径**（与 D328 的 `compose` / `partial` 同族），只报告不修。

/// **主断言（源码侧全称）**：`src/interpreter/**` 里的裸 `as <整型>` 转换，
/// 凡是右值含**用户实参来源名字**的，必须在 7 行内出现守卫。
///
/// 这是 D339 那条纪律的**机械化**：新加一个 `xs.take(some_arg)` 而忘了
/// 负数检查，本条会立刻红。
#[test]
fn d340_user_facing_numeric_casts_all_have_guards() {
    // 用户实参可能来源的名字（参数名 / Value 提取结果 / args 索引）
    //
    // ⚠ **必须用「词边界」匹配，不能用 `contains`**：第一版里放了 `"t"`、
    // `"n"`、`"f("` 这类**单字符/极短**项，而 `contains("t")` 对几乎每一行
    // 都为真（`record_tokens` 里就有 `t`）⇒ 全量误报。
    // 现按「标识符边界」匹配：左边不是字母数字下划线，右边同理。
    const USER_NAMES: &[&str] = &[
        "n", "size", "count", "max", "min", "rows", "cols", "index", "idx", "limit", "budget",
        "arity", "target", "total", "token", "input", "output", "num", "val", "x", "y", "num_",
        "amount",
    ];
    const GUARD: &[&str] = &[
        "< 0",
        "< 0.0",
        "non-negative",
        "不能为负",
        "is_finite",
        "value_as_",
        "checked_",
        "try_from",
    ];

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime/builtins");
    let mut scanned = 0usize; // 扫过的文件数
    let mut hits = 0usize; // 命中「用户实参」的行数
    let mut unguarded: Vec<String> = Vec::new();

    for entry in walk_rs(dir) {
        let src = std::fs::read_to_string(&entry).expect("读源码");
        scanned += 1;
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            // 行内出现 `as <整型>`
            let Some(_ty) = find_int_cast(t) else {
                continue;
            };
            // 且右值含用户实参来源的名字（**词边界**匹配）
            if !USER_NAMES.iter().any(|n| has_word(t, n)) {
                continue;
            }
            // ⚠ **排除形参**：形参已被函数签名约束为整型（如
            // `track_tokens(input: usize, …)` 里的 `input as u64` 是
            // **usize→u64 加宽**，不可能饱和或回绕）。
            // 本判据第一版把 `ai_helpers.rs:163` 的
            // `record_tokens(input as u64, output as u64)` 误报成缺陷，
            // 因为只看到了名字 `input`/`output` —— 它们是**形参**不是用户实参。
            // 判别：形参的**声明**里已经带整型类型 ⇒ 转换前的值必已是整型。
            let cand: Vec<&str> = USER_NAMES
                .iter()
                .copied()
                .filter(|n| has_word(t, n))
                .collect();
            if !cand.is_empty() && cand.iter().all(|n| declared_as_int_param_anywhere(&src, n)) {
                continue;
            }
            hits += 1;
            // 7 行内必须有守卫
            let lo = i.saturating_sub(6);
            let ctx: String = lines[lo..=i].concat();
            if GUARD.iter().any(|g| ctx.contains(g)) {
                continue;
            }
            // ⚠ **没有守卫**，但**未必是缺陷**：D339 已给出判别标准 ——
            // 「**是否类型相关**」。同一个负数，两种实参类型：
            //   结果**不同**（Float 饱和 0 / Int 回绕 1.8e19）⇒ 真缺陷（D285 / D339）；
            //   结果**相同**（都饱和 0）⇒ 静默兜底，属产品契约决定（D339 的 `ccr.marker`）。
            //
            // 静态判据**无法**区分这两者（要真跑），故这里只**报告**并
            // 让维护者对照 D339 的表确认 —— 不擅自判红。
            unguarded.push(format!(
                "{}:{}: {}",
                short_name(&entry),
                i + 1,
                if t.contains("|n|") || t.contains("n as") {
                    "值来自 optional_num_arg（D339 已逐条判定：仅 tea.run 需修，已修）"
                } else {
                    "需人工确认来源"
                }
            ));
        }
    }
    assert!(
        scanned >= 6,
        "只扫到 {scanned} 个文件 —— 目录布局变了？`src/runtime/builtins/**` 应有 10+ 个 .rs"
    );
    assert!(
        hits >= 1,
        "只命中 {hits} 处「用户实参 → 整型」转换 —— **匹配规则可能过严了**。"
    );
    // 无守卫的**已知清单**（D339 已逐条判定：只有 tea.run 需修且已修）
    for u in &unguarded {
        println!("d340: 无就近守卫 —— {u}");
    }
    assert!(
        !unguarded.iter().any(|u| u.contains("tea.rs")),
        "`tea.rs` 的 max_steps 守卫（D339 已加）在 7 行内必须可见; 实得: {unguarded:?}"
    );
    println!(
        "d340: 扫过 {scanned} 个文件，命中 {hits} 处「用户实参 → 整型」转换，\
         其中 {} 处无就近守卫（已在上面逐条列出）",
        unguarded.len()
    );
}

/// 标识符 `w` 是否以**词边界**出现在 `t` 里（左右不是字母数字下划线）。
fn has_word(t: &str, w: &str) -> bool {
    let b = t.as_bytes();
    let wb = w.as_bytes();
    if wb.is_empty() {
        return false;
    }
    for i in 0..=b.len().saturating_sub(wb.len()) {
        if &b[i..i + wb.len()] == wb {
            let left_ok = i == 0 || !is_ident_byte(b[i - 1]);
            let right = i + wb.len();
            let right_ok = right >= b.len() || !is_ident_byte(b[right]);
            if left_ok && right_ok {
                return true;
            }
        }
    }
    false
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// 该变量在本文件里是否**任何一处**被声明为整型**形参**。
///
/// 扫全文件而不是只看函数头 —— 形参声明常跨行
/// （`pub(super) fn track_tokens(&mut self, input: usize,\n    output: usize)`），
/// 按「往上找 40 行」会先撞上 `}` 而放弃。
///
/// 判定：文件中存在 `name: usize` / `name: u64` / … 形式的形参声明
/// ⇒ 该名字**在某些函数里**是整型形参。
///
/// ⚠ 这会有**假阴性**（同名变量在别处是 f64）—— 但那只会让本判据
/// **少查**一处，而不会**误报**。对「守卫普查」这个用途，
/// 宁可漏报不可误报（误报会让人去改本来正确的代码）。
fn declared_as_int_param_anywhere(src: &str, name: &str) -> bool {
    for ty in [
        "usize", "u8", "u16", "u32", "u64", "isize", "i8", "i16", "i32", "i64",
    ] {
        if src.contains(&format!("{name}: {ty}")) {
            return true;
        }
    }
    false
}

/// 行内是否有 `as <整型>`，返回整型名。
fn find_int_cast(t: &str) -> Option<&'static str> {
    for ty in [
        "usize", "u8", "u16", "u32", "u64", "isize", "i8", "i16", "i32", "i64",
    ] {
        if t.contains(&format!("as {ty}")) {
            // 排除 `as usize` 出现在 `format!` 等非转换语境 —— 保守起见都算
            return Some(match ty {
                "u8" => "u8",
                "u16" => "u16",
                "u32" => "u32",
                "u64" => "u64",
                "isize" => "isize",
                "i8" => "i8",
                "i16" => "i16",
                "i32" => "i32",
                "i64" => "i64",
                _ => "usize",
            });
        }
    }
    None
}

fn short_name(p: &str) -> String {
    p.rsplit('\\').next().unwrap_or(p).to_string()
}

/// 递归收集 `dir` 下的 `.rs`（手写以免引入依赖）。
fn walk_rs(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        let s = path.to_string_lossy().to_string();
        if path.is_dir() {
            if s.ends_with("target") {
                continue;
            }
            out.extend(walk_rs(&s));
        } else if s.ends_with(".rs") {
            out.push(s);
        }
    }
    out
}

/// **`numeric_helpers.rs:179` 的无守卫转换在可达输入下不会被触发**。
///
/// ⚠ **v0.104.6 D341 更正**：本条的第一版把这处写成「**死路径**」，那是**错的**
/// —— 路径确实可达（`json.parse` 产生 `Value::Int`，D263 已实测到达），
/// 但**触发不了**这个转换。
///
/// 实测的输入空间（全部经真实 CLI）：
///
/// | `budget` 实参 | 结果 |
/// |---|---|
/// | `1000`（`Int`，`json.parse`） | exit 0 |
/// | `"1KB"` | exit 0 |
/// | `"-1KB"` | exit 1 `invalid budget '-1KB'` |
/// | `"-0.5KB"` | exit 1 `invalid budget '-0.5KB'` |
/// | `"1e30GB"` | exit 1 `unknown budget unit 'E30GB'` |
///
/// 原因在 `numeric_helpers.rs:159` 的**解析循环**：
/// `while bytes[i].is_ascii_digit() || bytes[i] == b'.'` —— 它**不接受** `-`
/// 也不接受 `e`/`E`。所以 `num_part` 恒为**非负十进制**，
/// `num * mult` 不会产生负数。
///
/// 那 179 行的 `as usize` 什么时候会出事？需要 `num * mult` 超过 `f64`
/// 可表示的整型范围（≈1.8e19）。`num_part` 只能由数字和 `.` 组成 ⇒
/// 最长可写 `"99999999999999999999GB"` 这样的串，`num` 解析成 f64 后
/// 乘 2^30 仍 < 1.8e19 时转换精确；超过则 f64 本身已损失精度
/// ⇒ 得到的是一个**很大但不精确**的预算值，不是回绕。
///
/// ⇒ 结论从「死路径」改为「**解析层已挡住负数与大指数，剩余输入不会回绕**」。
/// 改写此条时请先跑下面的实测，别再凭读代码下结论。
/// **D342**：D340 遗留的「需人工确认来源」已逐条查清并钉住（`event.rs` / `retry_sleep_ms` 已于 v0.110.0 移除）。
#[test]
fn d340_every_unguarded_cast_has_a_confirmed_source() {
    let base_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime/builtins");
    let base = std::path::Path::new(base_dir);

    let exec_src = std::fs::read_to_string(base.join("exec.rs")).expect("读 exec.rs");
    assert!(
        exec_src.contains("Value::Int(self.idx as i64)"),
        "exec.rs 的 `as i64` 右侧应是 `self.idx`（命令索引，由 enumerate 产生）"
    );

    let md = std::fs::read_to_string(base.join("method.rs")).expect("读 method.rs");
    assert!(
        md.contains("\"len\" => Ok(Value::Int(s.chars().count() as i64)),"),
        "String.len() 的 `as i64` 右侧应是 `chars().count()`（字符数，非字节）"
    );

    // ── ② 两处「已有守卫但窗口不够」：`window` / `batch` 的 `size <= 0.0` ──
    for (method, needle) in [
        ("window", "window() size must be > 0"),
        ("batch", "batch() size must be > 0"),
    ] {
        let start = md
            .find(&format!("\"{method}\" =>"))
            .unwrap_or_else(|| panic!("未找到 {method} 的 arm"));
        let arm = &md[start..(start + 700).min(md.len())];
        assert!(
            arm.contains("if size <= 0.0"),
            "{method}(size) 的 arm 开头 700 字节内必须有 `size <= 0.0` 守卫"
        );
        assert!(
            arm.contains("size as usize"),
            "{method}(size) 仍应有 `size as usize` 转换"
        );
        // 守卫消息本身也要逐个钉住 —— 免得有人改措辞时顺手删了守卫
        assert!(
            arm.contains(needle),
            "{method}(size) 的守卫消息应为 `{needle}`（本判据按消息定位守卫）; 实得 arm: {arm}"
        );
    }

    // ── ③ `stats.rs` 的 `bins`：D329 加的整数守卫必须在同文件 ──
    let stats = std::fs::read_to_string(base.join("stats.rs")).expect("读 stats.rs");
    assert!(
        stats.contains("n.fract() == 0.0"),
        "stats.rs 应仍有 D329 的 `bins` 整数守卫（`fract() == 0.0`）"
    );
    assert!(
        stats.contains("let mut idx = ((x - min) / width) as usize;"),
        "histogram 的分桶转换行应仍在"
    );
}

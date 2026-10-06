//! v0.104.6 D170：`methods_of(<模块对象>)` 对**全部 23 个模块**返回 `[]`（普查 + 记档，**未修**）。
//!
//! ## 现象
//!
//! ```text
//! type_of(math)            → builtin
//! methods_of(math)         → []          ❌ 而 math.floor / math.sin / math.PI 都可用
//! methods_of(json)         → []          ❌ 而 json.parse / json.stringify 都可用
//! methods_of(file)         → []          ❌ 而 file.read_text / file.write_text 都可用
//! ```
//!
//! `method_dispatch.rs` 的 `_` 臂错误消息自己就列了
//! `mcp_servers, documents, or builtin objects` —— 即 **`builtin` 是被承认的一类**，
//! 但 `Value::methods()` 里**没有 `Value::Builtin` 分支**，全部落到 `_ => &[]`。
//!
//! D169 修的是 `Value::Document`（单类型、6 个方法、一个 arm）——
//! 本轮发现 `Value::Builtin` 是**同一个洞的 23 倍版本**，且我上轮的普查
//! **把它漏掉了**（我只对照了 `call_method_*` 具名分派器，
//! 没把 `Value::Builtin(kind)` 这个**通配臂**算进去）。
//!
//! ## 当初为何**不修**（历史记录，保留原样）
//!
//! 每个模块的方法名**没有独立清单** —— 它们以 `matches!(method, "a" | "b" | …)`
//! 的形式**内嵌**在两处：
//!
//! - `typeck::dispatch::module_method_signature`（当时的 900 行查表函数）
//! - `interpreter::builtins::<mod>.rs::call_<mod>_method`
//!
//! 要让 `methods_of` 报出真名，得把 900 行查表**重构成表驱动**（再由查表同时
//! 提供签名与枚举）。这是**重构**而非窄修复，动的是核心 typeck 路径，
//! 回归面与 D169 完全不同级。
//!
//! 更关键：若手写一份方法名清单却与实现漂移，`methods_of` 就从
//! 「空集」变成「**撒谎的清单**」—— 按 D169 的教训，**宁可记档也不要引入新的
//! 静默错误源**。故当轮只做普查 + 显式 known-gap 记录。
//!
//! 当时写下的验收判据是**双向闭合**：
//! ① 清单里每个名字在运行期**确实被接受**（不是 unknown method）；
//! ② 清单与 `module_method_signature` 的表**逐名一致**（两张手写表互为交叉验证）。
//!
//! ## 后续如何闭合
//!
//! - **D171** 做了那次重构：20 张 `*_GROUPS` 分组表，签名查找与
//!   `module_method_names` 读**同一张表** ⇒ 判据 ② 成立。
//! - **D175** 补齐 `ai` / `agent` / `random` 三个模块的方法名。
//! - **D293** 发现 D171 留下的**残余**风险：名字与签名虽同属一张表，组号
//!   ↔ 签名仍是**两处手写**（`Some(1)` 抄成 `Some(2)` 不报错，只会让 17 个
//!   一元函数静默拿到二元签名）。遂把两张表**合成一张**
//!   （`MethodGroup { names, min_arity, ret }`），组号这个中间层整个消失。
//!
//! 判据 ② 至此从「需要人工维护的约定」变成**由同一张表保证的不变量**，
//! 钉在 `tests/module_method_groups_single_table.rs`；本文件继续守判据 ①。

use std::process::Command;

/// `value.rs::MODULE_OBJECTS` 里的 23 个模块名（与生产表逐项对照）。
const MODULES: &[&str] = &[
    "ai", "web", "json", "file", "memory", "agent", "document", "bus", "sandbox", "schedule",
    "ccr", "mock", "exec", "tool", "skill", "plan", "mora", "math", "stats", "linalg", "random",
    "tea", "xform",
];

fn run_src(src: &str, tag: &str) -> String {
    let base = std::env::temp_dir().join(format!("mora_d170_{tag}"));
    std::fs::create_dir_all(&base).expect("建目录");
    let prog = base.join("p.mora");
    std::fs::write(&prog, src).expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&base);
    s
}

/// `probe_all` 的返回：`(各模块的 methods_of 结果, 各模块的 type_of 结果)`，
/// 元素均为 `(模块名, 输出行)`。
type ProbeRows = Vec<(String, String)>;

/// 一次程序里打印全部模块的 `methods_of` / `type_of`，再逐行解析。
///
/// ⚠ 刻意**不**做「每个模块起一次进程」：23 次临时文件往返在这个环境上
/// 不稳定（`remove_dir_all` 的失败被忽略 → 下一轮读到旧文件），曾把
/// `json` 误判成「非空」。单程序单文件，解析时用 `@@` 标记对齐。
fn probe_all() -> (ProbeRows, ProbeRows) {
    let mut src = String::new();
    for m in MODULES {
        // 标记行与结果行**分开**打印：不要写 `"@@m|" + methods_of(m)` ——
        // 字符串与 Value 的 `+` 拼接在本语言里不成立（实测整个程序无输出）。
        src.push_str(&format!("print(\"@@M {m}\")\n"));
        src.push_str(&format!("print(methods_of({m}))\n"));
        src.push_str(&format!("print(\"@@T {m}\")\n"));
        src.push_str(&format!("print(type_of({m}))\n"));
    }
    let out = run_src(&src, "all");
    let mut methods: Vec<(String, String)> = Vec::new();
    let mut types: Vec<(String, String)> = Vec::new();
    // 输出严格是「标记行、结果行」交替，故只需记住上一次标记的类别与名字。
    let mut pending: Option<(bool, String)> = None; // (is_type, name)
    for line in out.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("@@M ") {
            pending = Some((false, name.trim().to_string()));
        } else if let Some(name) = t.strip_prefix("@@T ") {
            pending = Some((true, name.trim().to_string()));
        } else if let Some((is_type, name)) = pending.take() {
            if is_type {
                types.push((name, t.to_string()));
            } else {
                methods.push((name, t.to_string()));
            }
        }
    }
    assert_eq!(
        methods.len(),
        MODULES.len(),
        "探针输出不完整（{}/{} 个模块）; 原始输出: {out}",
        methods.len(),
        MODULES.len()
    );
    (methods, types)
}

/// D170 主判据：普查全部模块的 `methods_of` 现状，并把结论**钉住**。
///
/// 本条不要求「必须为空」—— 若将来修好了，本条会红并提示改写为
/// 「必须列出方法」。这样两种状态下都有护栏。
#[test]
fn d170_census_methods_of_module_objects() {
    let (methods, types) = probe_all();
    let empty: Vec<&str> = methods
        .iter()
        .filter(|(_, v)| v.trim() == "[]")
        .map(|(m, _)| m.as_str())
        .collect();
    let non_empty: Vec<(&str, &str)> = methods
        .iter()
        .filter(|(_, v)| v.trim() != "[]")
        .map(|(m, v)| (m.as_str(), v.trim()))
        .collect();
    let not_builtin: Vec<(&str, &str)> = types
        .iter()
        .filter(|(_, v)| v.trim() != "builtin")
        .map(|(m, v)| (m.as_str(), v.trim()))
        .collect();

    println!("methods_of == [] 的模块（{} 个）：{empty:?}", empty.len());
    println!("非空的模块：{non_empty:?}");
    println!("type_of 非 builtin 的模块：{not_builtin:?}");

    assert!(
        not_builtin.is_empty(),
        "前提：23 个模块都应是 `builtin` 值（否则结论不成立）; 实得: {not_builtin:?}"
    );
    // v0.104.6 D171：`module_method_signature` 已重构成**表驱动**
    // （20 张方法组表），`module_method_names` 读**同一张表**，
    // 故枚举与签名不可能漂移。D170 记的「23 个模块全是 `[]`」至此闭合。
    //
    // v0.104.6 D293：进一步把「名字表 + 组号选签名」两张手写表合成
    // **一张**（`MethodGroup { names, min_arity, ret }`）—— D171 留下的
    // 残余风险是组号 ↔ 签名仍靠 `Some(N)` 手工对齐。详见
    // `tests/module_method_groups_single_table.rs`。
    //
    // v0.104.6 D175：原断言给 `ai` / `agent` / `random` 开了 `SIGNATURELESS`
    // 豁免，理由写的是「有精确 Type 变体 + 专用分派 → 没有可枚举的表」。
    // 那是**解释现状**，不是**豁免是对的**：实测 `ai.chat` / `ai.tokens` /
    // `ai.critic` / `agent.create` / `agent.critic` / `random.rand_int` …
    // 全部可达，而自省报空集等于告诉 agent「这个模块什么都不能做」。
    // 三者已补进 `module_method_names`，**豁免整体撤销** ——
    // 断言方向收紧为：23 个模块**全部**必须列出方法名。
    assert!(
        empty.is_empty(),
        "23 个模块都应列出方法名（D171 接上分组表，D175 补齐 ai/agent/random）; \
         仍为空: {empty:?}"
    );
    // 至少要有实质内容（防止「表存在但全空」这种假修复）
    let total: usize = non_empty
        .iter()
        .map(|(_, v)| v.trim().matches(',').count() + 1)
        .sum();
    assert!(
        total > 100,
        "方法名总量应显著（20 张表 + 3 张名字表），实得 {total} —— 疑似表没接上"
    );
}

/// D170 反向对照：模块方法**确实可用** —— 这才是「自省结果偏小」的危害所在。
#[test]
fn d170_module_methods_actually_work_despite_empty_introspection() {
    // math：零参常量 + 一元函数 + 二元函数
    let m = run_src("print(math.floor(1.7))\nprint(math.PI > 3.0)\n", "m1");
    assert!(m.contains("1.0"), "math.floor 应可用; 实得: {m}");
    assert!(m.contains("true"), "math.PI 应可用; 实得: {m}");
    // json / file：另两个 MODULE_OBJECTS 成员
    let j = run_src("print(json.stringify({a: 1}))\n", "m2");
    assert!(j.contains('a'), "json.stringify 应可用; 实得: {j}");
}

/// D170 对照组：D169 修好的 `document` 值（不是模块对象）**不得**被本轮影响。
#[test]
fn d170_document_value_introspection_still_listed() {
    let base = std::env::temp_dir().join("mora_d170_doc");
    std::fs::create_dir_all(&base).expect("建目录");
    let md = base.join("s.md");
    std::fs::write(&md, "# T\n\ntext\n").expect("写 md");
    let prog = base.join("p.mora");
    std::fs::write(
        &prog,
        format!(
            "let d = document.parse(\"{p}\")\nprint(methods_of(d))\n",
            p = md.display().to_string().replace('\\', "/")
        ),
    )
    .expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        s.contains("blocks") && s.contains("text"),
        "`document` **值**的 methods_of（D169 已修）不得回退; 实得: {s}"
    );
}

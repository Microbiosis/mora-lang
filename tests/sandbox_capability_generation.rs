//! v0.104.6 D388 —— sandbox 能力令牌层：`current_generation` **只写不读**，
//! 而 **5 处注释/测试理由声称 `check` 会校验 generation**（否定轮 + 一处注释漂移）
//!
//! `src/sandbox/capability.rs` 里同时存在两套互相矛盾的撤销机制描述：
//!
//! | 机制 | 状态 |
//! |---|---|
//! | `revoked: BTreeSet<u64>`（v0.49.0-fix P0-1）| **真实生效**，`check` 查它 |
//! | `current_generation` + `token.generation`（v0.49.0 A1/B1）| `revoke` 会 bump，但 **`check` 从不读** |
//!
//! ## 矛盾点：`check` 里没有 generation
//!
//! `check` 的函数体（`capability.rs:269-290`）只做三件事：
//! 查 `by_id` → 查 `revoked` → `token.permits()`。
//! **没有任何一行比较 `token.generation` 与 `inner.current_generation`。**
//!
//! 但下列 5 处都声称它有：
//!
//! | 位置 | 声称 |
//! |---|---|
//! | `capability.rs:200-202` | 「`check` requires `token.generation == current_generation`（else TokenNotFound）」 |
//! | `capability.rs:220-222` | 「Tokens with `generation != current_generation` are treated as not-found」 |
//! | `capability.rs:268` | 「同时校验 generation (A1)」 |
//! | `capability.rs:292-294` | 「旧持有者的 token 仍携带旧 generation, `check` 会视为 TokenNotFound」 |
//! | `tests/capability.rs:158-159` | 「TokenNotFound, 因为 token.generation != current_generation」 |
//!
//! ## 为什么**不该**去「实现」generation 校验
//!
//! 若真按注释加上全局 generation 校验，**per-token 撤销会退化成全局撤销**：
//! `revoke` bump 的是**全局**代数，同代签发的其它令牌会被连坐拒绝。
//! 而 P0-1 引入 `revoked` 集合正是为了修掉这个问题。
//!
//! ⇒ **代码是对的（per-token），注释是旧的**。这与 D365「注释长期停在
//! 旧普查数字」同型：注释描述的是 v0.49.0 A1 阶段、被 v0.49.0-fix P0-1
//! 取代却没同步。本轮**修注释**（注释漂移可修，D365 先例），
//! **不动代码**。
//!
//! ## 实测（真实 CLI，见 `sandbox_capability.mora`）
//!
//! ```text
//! a_before=true  b_before=true
//! sandbox.revoke(a)
//! a_after=false  b_after=true   ← 同代 b 未被连坐 ⇒ generation 未被强制
//! c_after_revokes=true          ← 撤销后新签发的令牌正常
//! ```
//!
//! 另附两项本轮顺带查清的事实：
//!
//! - `sandbox.key` **既未登记**在 typeck 的 `SANDBOX_METHODS`、**也不出现在**
//!   `methods_of(sandbox)`，但真实 CLI 跑得通 ⇒ 对**自省**低报了一个可调用、
//!   且是脚本侧唯一签发入口的方法。**不修**（`key` 是变参，`MethodGroup`
//!   只登记单一元数）⇒ 列为待裁决。
//! - capability **非法名**（如 `net.http`）在 builtin 层 `Capability::parse`
//!   就失败 ⇒ 抛 Err **终止脚本**（exit 1）；而**合法名但未授权**返回 `false`。

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use mora::sandbox::{Capability, CapabilityStore, SandboxError};

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

fn run_fixture(name: &str) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/e2e")
        .join(name);
    let out = Command::new(exe)
        .args(["run", script.to_str().expect("路径转字符串")])
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

/// 从 `text` 里截出 `start_marker` 到 `end_marker` 之间的片段。
fn slice_between(text: &str, start_marker: &str, end_marker: &str) -> String {
    let s = text
        .find(start_marker)
        .unwrap_or_else(|| panic!("找不到起点 {start_marker:?}"));
    let e = text[s..]
        .find(end_marker)
        .unwrap_or_else(|| panic!("找不到终点 {end_marker:?}"));
    text[s..s + e].to_string()
}

/// 剥掉注释行，只留**代码**。
///
/// ⚠ 首版没剥 —— 切片到下一个 `pub fn` 之前，会把**下一个** item 的
/// `/// doc comment` 一并带上，而那段 doc comment 里恰好出现 `generation`
/// ⇒ 判据把自己要否定的结论判成了「存在」。本判据要断的是
/// 「**代码**里没有 generation 比较」，注释不算。
fn code_only(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn store_with(caps: &[Capability]) -> (CapabilityStore, u64) {
    let store = CapabilityStore::new();
    let id = store
        .issue(caps.iter().copied().collect::<BTreeSet<_>>(), None)
        .expect("issue 应成功");
    (store, id)
}

// ── ① 库级：`check` 忽略 `token.generation`（本文件的核心事实）──

/// **stale generation 的令牌照样放行** —— 直接证明 `check` 不读 generation。
///
/// 构造：T0 / T1 同代（gen 0）签发 → 撤销 T1（全局代数 0→1）
/// → T0 的 `generation` 仍是 0，**已 stale**，但 `check(T0)` 仍 `Ok`。
///
/// 若 `check` 真按注释校验代数，本条会红（会返回 `TokenNotFound`）。
#[test]
fn d388_check_ignores_token_generation() {
    let (store, t0) = store_with(&[Capability::FileRead]);
    let t1 = store
        .issue(BTreeSet::from([Capability::FileWrite]), None)
        .expect("issue 应成功");

    assert_eq!(store.current_generation(), 0, "初始全局代数应为 0");
    assert_eq!(t0, 0);
    assert_eq!(t1, 1);
    // 两者同代签发
    assert_eq!(store.get(t0).unwrap().generation, 0);
    assert_eq!(store.get(t1).unwrap().generation, 0);

    store.revoke(t1).expect("revoke 应成功");
    assert_eq!(store.current_generation(), 1, "revoke 应 bump 全局代数");

    // T0 的 generation 仍为 0，与 current_generation(=1) **失配**
    let t0_gen = store.get(t0).unwrap().generation;
    assert_eq!(t0_gen, 0, "T0 自身的 generation 不会被 revoke 改动");
    assert_ne!(
        t0_gen,
        store.current_generation(),
        "前提：T0 的代数此刻确实 stale"
    );

    // 核心断言：stale 代数**不**导致拒绝
    assert_eq!(
        store.check(t0, Capability::FileRead),
        Ok(()),
        "T0 与全局代数失配却仍被放行 ⇒ `check` 确实不读 `token.generation`"
    );
}

/// **反向对照**：被撤销的那个令牌**确实**失效。
///
/// 只钉「stale 代数仍放行」是不够的 —— 那也可能是因为 revoke 根本没生效。
#[test]
fn d388_revoked_token_is_rejected() {
    let (store, id) = store_with(&[Capability::FileRead]);
    assert_eq!(store.check(id, Capability::FileRead), Ok(()));
    store.revoke(id).expect("revoke 应成功");
    assert_eq!(
        store.check(id, Capability::FileRead),
        Err(SandboxError::TokenNotFound { token_id: id }),
        "撤销后必须拒绝；错误类型是 TokenNotFound（走 `revoked` 集合，不是代数）"
    );
}

/// **per-token 撤销**：撤销 a 不波及同代的 b。
///
/// 这是「不该去实现注释里那套全局代数」的**实证依据** ——
/// 加上全局代数校验，b 会被连坐拒绝。
#[test]
fn d388_revoke_is_per_token_not_global() {
    let (store, a) = store_with(&[Capability::FileRead]);
    let b = store
        .issue(BTreeSet::from([Capability::FileWrite]), None)
        .expect("issue 应成功");

    store.revoke(a).expect("revoke 应成功");

    assert_eq!(
        store.check(a, Capability::FileRead),
        Err(SandboxError::TokenNotFound { token_id: a })
    );
    assert_eq!(
        store.check(b, Capability::FileWrite),
        Ok(()),
        "同代 b 不应被连坐；本条红 ⇒ 撤销已退化成全局语义"
    );
    // 撤销**不删**令牌（loongclaw 风格），只标记
    assert!(store.get(a).is_some(), "revoke 后令牌仍在 store 里");
}

/// **代数是只写状态**：`issue` 读它、`revoke` 写它，除 accessor 外无人消费。
///
/// 期望值来自**实测**（`Select-String` 扫非注释行）：生产区恰好 5 行提到
/// `current_generation`，且其中**只有 1 行是写**。
#[test]
fn d388_generation_is_write_only() {
    let src = read("src/sandbox/capability.rs");
    let cut = src.find("#[cfg(test)]").expect("应能找到单测区起点");
    let prod = &src[..cut];

    // 核心：`check` 函数体里不得出现 generation 比较
    let check_body = code_only(&slice_between(
        prod,
        "pub fn check(&self, token_id: u64, capability: Capability)",
        "pub fn revoke(",
    ));
    assert!(
        !check_body.contains("generation"),
        "`check` 的**代码**里出现了 generation —— 全局代数校验已被实现，\
         本文件关于「注释过期」的结论需重写。实得片段:\n{check_body}"
    );

    let mentions: Vec<&str> = prod
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("current_generation"))
        .filter(|l| !l.starts_with("//"))
        .collect();
    assert_eq!(
        mentions.len(),
        5,
        "生产区提及 `current_generation` 的行应恰好 5 行\
         （字段声明 / accessor 声明 / accessor 读取 / issue 读取 / revoke 写入）; \
         实得 {mentions:?} —— 多出来的说明它已被真正消费"
    );
    let writes = mentions
        .iter()
        .filter(|l| l.contains("inner.current_generation ="))
        .count();
    assert_eq!(
        writes, 1,
        "`current_generation` 应**只有 revoke 一处写入**; 实得 {writes} 处 —— \
         多出来说明存在本判据未覆盖的写路径"
    );
    // 读点：accessor 自身 + issue 给令牌打代数
    assert!(
        mentions
            .iter()
            .any(|l| l.contains("generation: inner.current_generation")),
        "`issue` 应把当前代数盖到新令牌上; 实得 {mentions:?}"
    );
}

// ── ② 源码级：把 5 处过期注释钉住（防止再次漂移）──

/// **`check` 实际查的是 `revoked` 集合** —— 与 v0.49.0-fix P0-1 注释一致。
#[test]
fn d388_check_consults_revoked_set() {
    let src = read("src/sandbox/capability.rs");
    let check_body = code_only(&slice_between(
        &src,
        "pub fn check(&self, token_id: u64, capability: Capability)",
        "pub fn revoke(",
    ));
    assert!(
        check_body.contains("inner.revoked.contains(&token_id)"),
        "`check` 应查 `revoked` 集合；实得片段:\n{check_body}"
    );
}

/// **`sandbox.key` 既没登记在 typeck、又对自省隐身，但真实 CLI 跑得通。**
///
/// 这是「typeck 表不完整」+「自省低报」两项现状钉。实测：
///
/// ```text
/// methods_of(sandbox) → [mode, check_builtin, check_path, check_call, revoke,
///                        token_count, audit_emit, audit_flush, audit_verify,
///                        containerize, container_exec, container_info, container_clear]
/// ```
///
/// —— 13 个，**没有 `key`**；而 `sandbox.key("file.read")` 实测可用，
/// 且它是脚本侧**唯一**签发令牌的入口（`revoke` / `check_call` 都以它为前提）。
///
/// **不修的理由**：`key` 是**变参**的（`for arg in args` 收 0..N 个 capability），
/// 而 `MethodGroup` 每项只登记**单一**元数。登记 1 会让 `key()` / `key(a,b)`
/// 在 typeck 里撒谎，登记不了就是现状 —— 这属**设计取舍**，按 D175 先例
/// （「把调不通的方法列进来，比空集更坏」的反面）只报告不擅动。
#[test]
fn d388_sandbox_key_callable_but_invisible_to_introspection() {
    let dispatch = read("src/typeck/dispatch.rs");
    let table = slice_between(&dispatch, "const SANDBOX_METHODS", "const MEMORY_METHODS");
    assert!(
        !table.contains("\"key\""),
        "`SANDBOX_METHODS` 已登记 `key` —— 本条关于「表不完整」的结论过期"
    );
    let sandbox_builtin = read("src/interpreter/builtins/sandbox.rs");
    assert!(
        sandbox_builtin.contains("\"key\" =>"),
        "`builtins/sandbox.rs` 应仍实现 `key` 分支"
    );

    // 反向对照：`key` 确实**可调用**（否则「自省低报」就只是「本来就没这功能」）
    let (code, out) = run_fixture("sandbox_capability.mora");
    assert_eq!(code, 0, "fixture 应成功; out={out}");
    assert!(
        out.contains("before=true"),
        "`sandbox.key` 应可用; out={out}"
    );

    // 自省里 `key` 缺席（现状钉）
    let base = std::env::temp_dir().join("mora_d388_introspect");
    let _ = std::fs::create_dir_all(&base);
    let prog = base.join("p.mora");
    std::fs::write(&prog, "print(methods_of(sandbox))").expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&prog).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&base);
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = s
        .lines()
        .find(|l| l.trim_start().starts_with('['))
        .unwrap_or_else(|| panic!("未取到 methods_of 输出; out={s}"));
    assert!(
        !line.contains("key"),
        "`methods_of(sandbox)` 已列出 `key` —— 自省低报已修，本条结论需重写; line={line}"
    );
    // 但表里登记的 13 个方法**都**在（确保不是整体空表）
    for m in ["check_call", "revoke", "token_count", "container_clear"] {
        assert!(line.contains(m), "自省应仍含 `{m}`; line={line}");
    }
}

// ── ③ 端到端：真实 CLI 路径 ──

/// **per-token 撤销在脚本层同样成立**（`a` 死、`b` 活）。
#[test]
fn d388_e2e_revoke_is_per_token() {
    let (code, out) = run_fixture("sandbox_capability.mora");
    assert_eq!(code, 0, "fixture 应成功; out={out}");
    let expect = [
        ("before=true", "单令牌撤销前放行"),
        ("after=false", "单令牌撤销后拒绝"),
        ("a_before=true", "a 撤销前放行"),
        ("b_before=true", "b 撤销前放行"),
        ("a_after=false", "a 被撤销"),
        ("b_after=true", "同代 b 未被连坐"),
        ("c_after_revokes=true", "撤销后新签发的令牌正常"),
        ("d_web=false", "合法 capability 名但未授权 ⇒ false"),
    ];
    for (needle, why) in expect {
        assert!(
            out.contains(needle),
            "缺 `{needle}`（{why}）; 实际输出:\n{out}"
        );
    }
    // 撤销不删令牌 ⇒ 5 次签发全在
    assert!(out.contains("count=5.0"), "令牌数应仍为 5.0; out={out}");
}

/// **非法 capability 名抛错终止**，且消息**点名**那个名字。
///
/// 与上面 `d_web=false` 配对：合法名未授权 = 返回 `false`；
/// 非法名 = builtin 层就失败 ⇒ 脚本终止。
#[test]
fn d388_e2e_bad_capability_name_errors_and_names_it() {
    let (code, out) = run_fixture("sandbox_bad_capability.mora");
    assert_ne!(code, 0, "非法 capability 名应非零退出; out={out}");
    assert!(
        out.contains("unknown capability 'net.http'"),
        "错误消息应点名非法名; out={out}"
    );
    assert!(
        !out.contains("unreachable"),
        "抛错后不应继续执行后续语句; out={out}"
    );
}

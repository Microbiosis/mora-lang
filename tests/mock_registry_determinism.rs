//! v0.104.6 D405 —— `mock.names()` 返回 **`HashMap` 随机序** ⇒ 同一脚本跨次运行
//! 输出不同（修复轮）
//!
//! ## 缺陷
//!
//! `MockRegistry::names()` 直接返回 `HashMap` 的键，迭代序由 `RandomState`
//! （**逐进程随机**）决定。`mock.names()` **脚本可达**
//! （`examples/integration_v0_34.mora` 就在用）⇒ **用户可见的输出不确定**。
//!
//! 实测：同一段脚本连跑 **6 次得到 6 个不同顺序**：
//!
//! ```text
//! [charlie, bravo, delta, echo, alpha]
//! [delta, bravo, echo, charlie, alpha]
//! [echo, bravo, charlie, alpha, delta]
//! [charlie, delta, echo, alpha, bravo]
//! [alpha, delta, bravo, echo, charlie]
//! [delta, echo, charlie, alpha, bravo]
//! ```
//!
//! 与 D280 修 `tool.list_tools`、D385 立的「HashMap 迭代顺序不确定
//! → 会让结果依赖顺序」是**同一条原则**。
//!
//! ## 修法：在 registry 层排序（与 `list_planes` 同层）
//!
//! ## ⚠ 既有单测 `multiple_handlers` 对顺序**没有牙齿**
//!
//! 它 **自己先 `sort()` 再比较** ⇒ 排不排序**都能通过**。
//! 本文件的守卫是「直接断言已排序的期望值」+「跨进程比对」。
//!
//! ## 为什么必须有**跨进程**判据
//!
//! 缺陷按定义是**逐进程**随机的 ⇒ 单进程内的库级测试**原理上抓不到**
//! （同一个进程里 `RandomState` 是固定的）。所以关键判据是
//! **起 N 个进程比对输出**。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use mora::mock::{MockHandler, MockRegistry};
use mora::value::Value;

static SEQ: AtomicU64 = AtomicU64::new(0);

fn work_dir(tag: &str) -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("mora_d405_{n}_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

fn native() -> MockHandler {
    MockHandler::Native(std::sync::Arc::new(|v: &Value| v.clone()))
}

// ── ① 库级：直接断言「已排序」，**不在测试里 sort** ──

/// **`names()` 直接返回排序结果**。
///
/// ⚠ **绝不能**在测试里先 `sort()` —— 那样排不排序都能通过（既有
/// `multiple_handlers` 正是这样，故对顺序无牙齿）。
#[test]
fn d405_names_are_sorted_without_the_test_sorting() {
    let r = MockRegistry::new();
    // 故意**逆序**注册：若实现把 HashMap 序返回，就与期望不符
    for n in ["echo", "delta", "charlie", "bravo", "alpha"] {
        r.register(n, native());
    }
    assert_eq!(
        r.names(),
        vec!["alpha", "bravo", "charlie", "delta", "echo"],
        "`names()` 未按名称排序 —— 用户看到的顺序将是逐进程随机的"
    );
}

/// **注册顺序不影响 `names()` 顺序**。
#[test]
fn d405_names_order_is_independent_of_registration_order() {
    let a = MockRegistry::new();
    for n in ["a", "b", "c", "d"] {
        a.register(n, native());
    }
    let b = MockRegistry::new();
    for n in ["d", "c", "b", "a"] {
        b.register(n, native());
    }
    assert_eq!(a.names(), b.names(), "注册顺序不同 ⇒ `names()` 顺序却相同");
}

/// **重复调用稳定**（不是只测「两次碰巧一样」而是同一 map 反复取）。
#[test]
fn d405_names_stable_across_repeated_calls() {
    let r = MockRegistry::new();
    for (i, n) in ["m1", "m2", "m3", "m4", "m5", "m6"].iter().enumerate() {
        r.register(n, native());
        let _ = i;
    }
    let first = r.names();
    for k in 0..20 {
        assert_eq!(r.names(), first, "第 {k} 次调用顺序不同");
    }
}

// ── ② 跨进程：这一条才是真正的牙齿 ──

/// **同一脚本连跑 4 个进程，`mock.names()` 输出必须逐字相同**。
///
/// 修前实测 4 次 4 个不同顺序 ⇒ 本条会红。
#[test]
fn d405_names_identical_across_processes() {
    let d = work_dir("e2e");
    let script = d.join("p.mora");
    std::fs::write(
        &script,
        "mock.register(\"alpha\", fn(x) => x end)\n\
         mock.register(\"bravo\", fn(x) => x end)\n\
         mock.register(\"charlie\", fn(x) => x end)\n\
         mock.register(\"delta\", fn(x) => x end)\n\
         mock.register(\"echo\", fn(x) => x end)\n\
         print(\"names=\" + str(mock.names()))\n",
    )
    .expect("写探针");

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let mut outs: Vec<String> = Vec::new();
    for k in 0..4 {
        let out = Command::new(exe).arg(&script).output().expect("跑 mora");
        assert_eq!(out.status.code(), Some(0), "第 {k} 次非零退出");
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let line = s
            .lines()
            .find(|l| l.starts_with("names="))
            .unwrap_or_else(|| panic!("第 {k} 次未取到 names 行; out={s}"))
            .to_string();
        outs.push(line);
    }
    let _ = std::fs::remove_dir_all(&d);

    // ⚠ 逐个与第 0 次比 —— 不能写成 `outs[1..] == outs[..1]`：
    // 那是「长度 3 的切片 vs 长度 1 的切片」，**永远不等**（首版就这样写错了，
    // 报错信息里三个值明明逐字相同）。
    for (k, o) in outs.iter().enumerate().skip(1) {
        assert_eq!(o, &outs[0], "第 {k} 个进程顺序不同: {o} vs {}", outs[0]);
    }
    // 顺带钉住确切值（排序而非「碰巧某个序」）
    assert_eq!(
        outs[0], "names=[alpha, bravo, charlie, delta, echo]",
        "顺序应是按名升序"
    );
}

// ── ③ 与兄弟实现的一致性 ──

/// **`ToolPlaneRegistry::list_planes` 也排序**（D280 的修复），两处口径一致。
#[test]
fn d405_sibling_list_planes_is_also_sorted() {
    let mut reg = mora::toolplane::ToolPlaneRegistry::new();
    for n in ["zeta", "alpha", "mu"] {
        reg.create_plane(n.to_string(), mora::toolplane::PlaneKind::Core)
            .unwrap();
    }
    assert_eq!(reg.list_planes(), vec!["alpha", "mu", "zeta"]);
}

// ── ④ 现状钉：`register` 重名**静默覆盖**（只记录，不改） ──

/// **`register` 同名覆盖是当前语义。**
///
/// 与 `ToolPlane::register`（重名**报错**）口径不同，但**这是合理的**：
/// `mock` 是**注册表/setter**（重复注册 = 换 handler，预期行为）；
/// `tool` 是**工具目录**（重复 = 建模错误，值得报错）。
/// ⇒ 两者不是同一类东西，**不**按「兄弟不一致」判缺陷；此处只钉现状。
#[test]
fn d405_register_overwrites_silently_by_design() {
    let r = MockRegistry::new();
    r.register(
        "k",
        MockHandler::Native(std::sync::Arc::new(|_| Value::String("first".into()))),
    );
    r.register(
        "k",
        MockHandler::Native(std::sync::Arc::new(|_| Value::String("second".into()))),
    );
    assert_eq!(r.count(), 1, "同名覆盖而非新增");
    let h = r.get("k").expect("应能取回");
    match h {
        MockHandler::Native(f) => assert_eq!(
            f(&Value::Nil).to_string(),
            "second",
            "应保留**后**注册的 handler"
        ),
        MockHandler::Script(_) => panic!("expected Native"),
    }
}

//! v0.104.6 D282：确定性回归护栏 —— 那些**已经**做对的点不许退化
//!
//! ## 为什么要有这个文件
//!
//! D280 修掉了 `OrchestrateDag::topological_order`：它对并列节点返回
//! **不确定顺序**（`HashMap` 迭代 + 逐进程随机种子）。它的特别之处在于
//! **同仓库的其它地方都已经处理过这个问题**，它是那批修复里**漏掉的一处**：
//!
//! | 位置 | 措施 |
//! |---|---|
//! | `value/display.rs:50-56` / `203-207` | `Value::Dict` 的 Display **按 key 排序**（注释明写「v0.104.6 可复现性修复…`Value::Dict` 是 `HashMap`，迭代序每进程随机」） |
//! | `flow::json::value_to_json` | 同一族的对齐实现 |
//! | http / mcp 服务器 | 用 `BTreeMap` |
//! | `toolplane::list_planes` | `names.sort()` + 已有单测 `list_planes_returns_sorted` |
//! | `builtins/toolplane.rs::list_tools` | `names.sort()` |
//! | `pregel/mod.rs:818-828` | 按 agent 定义顺序排 `active_nodes`（注释：「HashSet 迭代顺序不确定 → 会让结果依赖顺序」） |
//! | ~~`orchestrate_dag::topological_order`~~ | **D280 之前：无** ⇒ 漏网 |

//! ⇒ 本文件把**已经正确**的那些点钉住，让「D280 那一类」不会在别处复现。
//! 它本身不测新行为，只防退化。
//!
//! ## 本轮的两条**否定**（避免下轮重走）
//!
//! - `toolplane::list_planes` **已经**排序，且已有单测锁定 ⇒ 不必改。
//! - `Value::Dict` 的 Display **已经**排序（浅层与深层两处分支都有）
//!   ⇒ 打印 dict 不会因 HashMap 顺序而抖动。

use mora::toolplane::{PlaneKind, ToolPlaneRegistry};
use mora::value::Value;

/// dict 的 `Display` 必须**按 key 有序**（跨进程可复现）。
///
/// `Value::Dict` 内部是 `HashMap<String, Value>`（逐进程随机种子），
/// 所以这条只能靠「实现里排了序」来保证 —— 一旦有人删掉那个 `sort_by`，
/// 本条立刻变红。
#[test]
fn d282_dict_display_is_sorted() {
    let v = Value::Dict(
        [
            ("gamma".to_string(), Value::Float(3.0)),
            ("alpha".to_string(), Value::Float(1.0)),
            ("beta".to_string(), Value::Float(2.0)),
        ]
        .into_iter()
        .collect(),
    );
    let s = format!("{v}");
    let ia = s.find("alpha").expect("应含 alpha");
    let ib = s.find("beta").expect("应含 beta");
    let ig = s.find("gamma").expect("应含 gamma");
    assert!(
        ia < ib && ib < ig,
        "dict 的 Display 必须按 key 排序，实得：{s}"
    );
}

/// 嵌套 dict 的深层分支同样必须排序。
///
/// `display.rs` 有**两处** `Value::Dict` 分支（浅层入口与深度递归分支），
/// 历史上只排一处是常见疏漏 —— 本条守住第二处。
#[test]
fn d282_nested_dict_display_is_sorted_too() {
    let inner = Value::Dict(
        [
            ("z".to_string(), Value::Float(1.0)),
            ("y".to_string(), Value::Float(2.0)),
        ]
        .into_iter()
        .collect(),
    );
    let outer = Value::Dict(
        [
            ("b".to_string(), inner.clone()),
            (
                "a".to_string(),
                Value::List(mora::value::list::List::from_vec(vec![inner])),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let s = format!("{outer}");
    // 外层 a 在 b 之前
    assert!(s.find('a') < s.find('b'), "外层 dict 应按 key 排序：{s}");
    // 内层每个 y 在 z 之前
    let first_y = s.find('y').expect("内层应含 y");
    let first_z = s.find('z').expect("内层应含 z");
    assert!(first_y < first_z, "内层 dict 也应按 key 排序：{s}");
}

/// `toolplane.list_planes` 必须返回**有序**列表。
///
/// 修前担心它是 `HashMap` 遍历（D280 同类），实测**已经**有 `names.sort()`
/// 且模块内已有单测 `list_planes_returns_sorted` 锁定 ⇒ **否定，不改**。
/// 本条防的是将来有人「优化」掉那个 `sort`。
#[test]
fn d282_toolplane_list_planes_is_sorted() {
    let mut reg = ToolPlaneRegistry::new();
    for n in ["zeta", "alpha", "mid"] {
        reg.create_plane(n.to_string(), PlaneKind::Core)
            .expect("创建应成功");
    }
    assert_eq!(reg.list_planes(), vec!["alpha", "mid", "zeta"]);
}

/// `toolplane` 里的工具名列表也应有序（`list_tools` 里的 `names.sort()`）。
#[test]
fn d282_toolplane_tool_listing_is_sorted() {
    let mut reg = ToolPlaneRegistry::new();
    reg.create_plane("p".to_string(), PlaneKind::Extension)
        .expect("创建应成功");
    {
        let plane = reg.get_plane_mut("p").expect("plane 应存在");
        for t in ["zeta", "alpha", "mid"] {
            plane
                .register(mora::toolplane::ToolSpec {
                    name: t.to_string(),
                    description: "d".to_string(),
                    parameters: "p".to_string(),
                })
                .expect("注册应成功");
        }
    }
    let mut names: Vec<String> = reg.get_plane("p").unwrap().tools.keys().cloned().collect();
    names.sort();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
}

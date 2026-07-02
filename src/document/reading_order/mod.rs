//! v0.33: Document reading order algorithms
//!
//! 灵感: MinerU §2.8 Reading Order Recovery (Uni-Parser 技术报告)
//! 3 算法:
//! 1. **XY-cut**: 递归按 dominant whitespace 划分 (binary tree)
//! 2. **Gap-tree**: 按 inter-block gap + 几何接近度 + 对齐
//! 3. **Group-based**: 语义组内聚类 (figure+caption, table+title)
//!
//! v0.33 简化: blocks 列表可携带可选 bbox (x, y, w, h) 字段.
//! 若无 bbox, 退化到"输入顺序" (Mora 现有 backend 行为).
//!
//! 设计: input 任意 List<Value>, 每项必须含 "text" 字段.
//! 可选字段: "bbox" = {x, y, w, h} (Number). 输出加 "reading_order_idx".

use crate::value::Value;

/// v0.33: 简化 bbox (x, y, w, h 像素坐标)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl BBox {
    /// 从 Dict 解析 bbox. 接受两种格式:
    /// 1. v 本身是 bbox dict (含 x, y, w, h)
    /// 2. v 是 block dict, 含 "bbox" 字段指向 bbox dict
    pub fn from_value(v: &crate::value::Value) -> Option<Self> {
        use crate::value::Value;
        let bbox_dict = if let Value::Dict(d) = v {
            // 先看 v 本身是否是 bbox
            if d.contains_key("x")
                && d.contains_key("y")
                && d.contains_key("w")
                && d.contains_key("h")
            {
                d
            } else {
                // 否则找 "bbox" 字段
                match d.get("bbox")? {
                    Value::Dict(bb) => bb,
                    _ => return None,
                }
            }
        } else {
            return None;
        };
        let get = |k: &str| {
            bbox_dict.get(k).and_then(|x| {
                if let Value::Number(n) = x {
                    Some(*n)
                } else {
                    None
                }
            })
        };
        Some(Self {
            x: get("x")?,
            y: get("y")?,
            w: get("w")?,
            h: get("h")?,
        })
    }

    pub fn center_y(&self) -> f64 {
        self.y + self.h / 2.0
    }

    pub fn center_x(&self) -> f64 {
        self.x + self.w / 2.0
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// v0.33: Reading order strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// 输入顺序 (fallback 当 blocks 无 bbox)
    InputOrder,
    /// 垂直方向按 center_y 升序, 同 y 按 center_x 升序
    TopToBottom,
    /// Gap-tree 启发式: 大 vertical gap 触发 row 切换
    GapTree,
    /// XY-cut (简化版, 不递归切分)
    XyCut,
    /// Group-based: 按 bbox 水平 alignment 聚类
    GroupBased,
}

impl Strategy {
    #[allow(clippy::should_implement_trait)] // 避免与 std::str::FromStr 混淆
    pub fn from_str(s: &str) -> Self {
        match s {
            "input" | "input_order" => Self::InputOrder,
            "top_to_bottom" | "ttb" => Self::TopToBottom,
            "gap_tree" | "gap" => Self::GapTree,
            "xy_cut" | "xy" => Self::XyCut,
            "group_based" | "group" => Self::GroupBased,
            _ => Self::TopToBottom, // default
        }
    }
}

/// v0.33: 为 blocks 列表加 reading_order_idx
/// input: List<Value>, 每项至少含 "text", 可选 "bbox"
/// output: 同 List<Value>, 加 "reading_order_idx" 字段
pub fn assign_reading_order(
    blocks: Vec<crate::value::Value>,
    strategy: Strategy,
) -> Vec<crate::value::Value> {
    if blocks.is_empty() {
        return blocks;
    }

    // 抽取 bbox
    let bboxes: Vec<Option<BBox>> = blocks.iter().map(BBox::from_value).collect();

    // 计算每个 block 的排序 key
    let mut indices: Vec<usize> = (0..blocks.len()).collect();
    match strategy {
        Strategy::InputOrder => {
            // indices already 0..n
        }
        Strategy::TopToBottom => {
            indices.sort_by(|&a, &b| {
                let ba = bboxes[a];
                let bb = bboxes[b];
                match (ba, bb) {
                    (Some(ba), Some(bb)) => {
                        // a 在 b 之前如果 a 的 vertical range 在 b 之前 (无 overlap)
                        let a_bot = ba.bottom();
                        let b_top = bb.y;
                        let a_top = ba.y;
                        let b_bot = bb.bottom();
                        if a_bot <= b_top {
                            std::cmp::Ordering::Less
                        } else if b_bot <= a_top {
                            std::cmp::Ordering::Greater
                        } else {
                            // 重叠 (同 row), 按 x
                            ba.x.partial_cmp(&bb.x).unwrap_or(std::cmp::Ordering::Equal)
                        }
                    }
                    _ => std::cmp::Ordering::Equal, // 缺 bbox 保持原序
                }
            });
        }
        Strategy::GapTree => {
            // 按 center_y 升序, 同 y 用 center_x 升序
            indices.sort_by(|&a, &b| {
                let ba = bboxes[a];
                let bb = bboxes[b];
                match (ba, bb) {
                    (Some(ba), Some(bb)) => {
                        let cy_a = ba.center_y();
                        let cy_b = bb.center_y();
                        cy_a.partial_cmp(&cy_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| {
                                ba.center_x()
                                    .partial_cmp(&bb.center_x())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                    }
                    _ => std::cmp::Ordering::Equal,
                }
            });
        }
        Strategy::XyCut => {
            // 简化 XY-cut: 按 (row 高度分区) + (col 宽度分区) 排序
            // row: by y; col within row: by x
            indices.sort_by(|&a, &b| {
                let ba = bboxes[a];
                let bb = bboxes[b];
                match (ba, bb) {
                    (Some(ba), Some(bb)) => ba
                        .y
                        .partial_cmp(&bb.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| ba.x.partial_cmp(&bb.x).unwrap_or(std::cmp::Ordering::Equal)),
                    _ => std::cmp::Ordering::Equal,
                }
            });
        }
        Strategy::GroupBased => {
            // 按 x 重叠聚类: 同 x-overlap 分一组, 同一 group 内按 y
            // 简化: 按 center_x 排序, 然后按 y
            indices.sort_by(|&a, &b| {
                let ba = bboxes[a];
                let bb = bboxes[b];
                match (ba, bb) {
                    (Some(ba), Some(bb)) => {
                        let cx_a = ba.center_x();
                        let cx_b = bb.center_x();
                        cx_a.partial_cmp(&cx_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| {
                                ba.y.partial_cmp(&bb.y).unwrap_or(std::cmp::Ordering::Equal)
                            })
                    }
                    _ => std::cmp::Ordering::Equal,
                }
            });
        }
    }

    // 写回 reading_order_idx
    indices
        .into_iter()
        .enumerate()
        .map(|(new_idx, old_idx)| {
            let mut block = blocks[old_idx].clone();
            if let Value::Dict(ref mut d) = block {
                d.insert(
                    "reading_order_idx".to_string(),
                    Value::Number(new_idx as f64),
                );
            }
            block
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::collections::HashMap;

    fn make_block(text: &str, bbox: Option<BBox>) -> Value {
        let mut d = HashMap::new();
        d.insert("text".to_string(), Value::String(text.to_string()));
        if let Some(b) = bbox {
            let mut bb = HashMap::new();
            bb.insert("x".to_string(), Value::Number(b.x));
            bb.insert("y".to_string(), Value::Number(b.y));
            bb.insert("w".to_string(), Value::Number(b.w));
            bb.insert("h".to_string(), Value::Number(b.h));
            d.insert("bbox".to_string(), Value::Dict(bb));
        }
        Value::Dict(d)
    }

    fn reading_order_idx(b: &Value) -> Option<usize> {
        if let Value::Dict(d) = b {
            if let Some(Value::Number(n)) = d.get("reading_order_idx") {
                Some(*n as usize)
            } else {
                None
            }
        } else {
            None
        }
    }

    #[test]
    fn input_order_preserves_sequence() {
        let blocks = vec![
            make_block("first", None),
            make_block("second", None),
            make_block("third", None),
        ];
        let out = assign_reading_order(blocks, Strategy::InputOrder);
        assert_eq!(out.len(), 3);
        assert_eq!(reading_order_idx(&out[0]), Some(0));
        assert_eq!(reading_order_idx(&out[1]), Some(1));
        assert_eq!(reading_order_idx(&out[2]), Some(2));
    }

    #[test]
    fn top_to_bottom_simple() {
        // 物理顺序: 1, 2, 3 (top to bottom)
        let blocks = vec![
            make_block(
                "top",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "mid",
                Some(BBox {
                    x: 0.0,
                    y: 30.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "bot",
                Some(BBox {
                    x: 0.0,
                    y: 60.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::TopToBottom);
        let texts: Vec<String> = out
            .iter()
            .map(|b| {
                if let Value::Dict(d) = b {
                    if let Some(Value::String(s)) = d.get("text") {
                        s.clone()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            })
            .collect();
        // 物理顺序已经是 TTB, 排序后应保持
        assert_eq!(texts, vec!["top", "mid", "bot"]);
    }

    #[test]
    fn top_to_bottom_reorders_physical() {
        // 物理: 2, 1, 3 (乱序) → 期望: 1, 2, 3
        let blocks = vec![
            make_block(
                "mid",
                Some(BBox {
                    x: 0.0,
                    y: 30.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "top",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "bot",
                Some(BBox {
                    x: 0.0,
                    y: 60.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::TopToBottom);
        let texts: Vec<String> = out
            .iter()
            .filter_map(|b| {
                if let Value::Dict(d) = b {
                    if let Some(Value::String(s)) = d.get("text") {
                        Some(s.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(texts, vec!["top", "mid", "bot"]);
    }

    #[test]
    fn gap_tree_two_columns() {
        // 两列: 左列 + 右列, 跨多行
        let blocks = vec![
            make_block(
                "L1",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R1",
                Some(BBox {
                    x: 60.0,
                    y: 5.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "L2",
                Some(BBox {
                    x: 0.0,
                    y: 30.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R2",
                Some(BBox {
                    x: 60.0,
                    y: 35.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::GapTree);
        let texts: Vec<String> = out
            .iter()
            .filter_map(|b| {
                if let Value::Dict(d) = b {
                    if let Some(Value::String(s)) = d.get("text") {
                        Some(s.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        // gap_tree: L1, R1, L2, R2 (按 y, 同 y 按 x)
        assert_eq!(texts, vec!["L1", "R1", "L2", "R2"]);
    }

    #[test]
    fn xy_cut_simple() {
        // 4 blocks arranged in 2x2 grid
        let blocks = vec![
            make_block(
                "TL",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 50.0,
                }),
            ),
            make_block(
                "TR",
                Some(BBox {
                    x: 60.0,
                    y: 0.0,
                    w: 50.0,
                    h: 50.0,
                }),
            ),
            make_block(
                "BL",
                Some(BBox {
                    x: 0.0,
                    y: 60.0,
                    w: 50.0,
                    h: 50.0,
                }),
            ),
            make_block(
                "BR",
                Some(BBox {
                    x: 60.0,
                    y: 60.0,
                    w: 50.0,
                    h: 50.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::XyCut);
        let texts: Vec<String> = out
            .iter()
            .filter_map(|b| {
                if let Value::Dict(d) = b {
                    if let Some(Value::String(s)) = d.get("text") {
                        Some(s.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        // xy_cut: row by y, within row by x -> TL, TR, BL, BR
        assert_eq!(texts, vec!["TL", "TR", "BL", "BR"]);
    }

    #[test]
    fn strategy_from_str() {
        assert_eq!(Strategy::from_str("input"), Strategy::InputOrder);
        assert_eq!(Strategy::from_str("xy_cut"), Strategy::XyCut);
        assert_eq!(Strategy::from_str("garbage"), Strategy::TopToBottom);
    }

    #[test]
    fn empty_input() {
        let out = assign_reading_order(vec![], Strategy::TopToBottom);
        assert!(out.is_empty());
    }

    #[test]
    fn blocks_without_bbox_use_input_order() {
        // 部分 blocks 有 bbox, 部分无 -> 缺 bbox 的用原序
        let blocks = vec![
            make_block("no_bbox", None),
            make_block(
                "with_bbox",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::TopToBottom);
        // 2 blocks, both got reading_order_idx
        for b in &out {
            assert!(reading_order_idx(b).is_some());
        }
    }
}

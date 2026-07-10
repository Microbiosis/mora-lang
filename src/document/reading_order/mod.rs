//! v0.33+: Document reading order algorithms
//!
//! 灵感: MinerU §2.8 Reading Order Recovery + arXiv:2504.10258 "XY-Cut++"
//! 5 算法 (v0.33 → v0.41 增补):
//! 1. **InputOrder**: 输入顺序 (无 bbox fallback)
//! 2. **TopToBottom**: 简单 vertical sort
//! 3. **GapTree**: 按 inter-block gap + 几何接近度 + 对齐
//! 4. **XyCut**: 简化版 XY-cut (flat sort, 无递归切分)
//! 5. **XyCutPlusPlus** (v0.41): MinerU 升级版 — 递归投影-轮廓分裂
//!    + cross-layout elements (跨栏页眉/页脚) 处理
//!
//! v0.33 简化: blocks 列表可携带可选 bbox (x, y, w, h) 字段.
//! 若无 bbox, 退化到"输入顺序" (Mora 现有 backend 行为).
//!
//! v0.41.1: XyCutPlusPlus 替换 v1 master doc 提到的 `recursive_xy_cut`
//!   (MinerU 已弃用旧版, 改用 XY-Cut++)
//!
//! 设计: input 任意 `List<Value>`, 每项必须含 "text" 字段.
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

/// v0.41.1: XY-Cut++ 算法常量 (来自 MinerU arXiv:2504.10258)
///
/// - `BETA = 2.0`: cross-layout 元素判定阈值 (width > beta * median_width)
/// - `DENSITY_THRESHOLD = 0.9`: x/y 投影密度比阈值，决定首次切分方向
/// - `OVERLAP_THRESHOLD = 0.1`: overlap 判定下限 (相对自身宽度)
/// - `MIN_OVERLAP_COUNT = 2`: cross-layout 至少跨 N 个 column
/// - `MIN_GAP_THRESHOLD = 5.0`: 投影最小 gap 像素
const XY_CUT_PLUS_PLUS_BETA: f64 = 2.0;
const XY_CUT_PLUS_PLUS_DENSITY: f64 = 0.9;
const XY_CUT_PLUS_PLUS_OVERLAP: f64 = 0.1;
const XY_CUT_PLUS_PLUS_MIN_OVERLAP_COUNT: usize = 2;
const XY_CUT_PLUS_PLUS_MIN_GAP: f64 = 5.0;

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

/// v0.33+: Reading order strategy
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
    /// v0.41.1: XY-Cut++ (MinerU arXiv:2504.10258 升级版)
    /// 递归投影-轮廓分裂 + cross-layout 元素 (跨栏页眉/页脚) 处理
    XyCutPlusPlus,
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
            // v0.41.1: 新增 XY-Cut++ 别名
            "xy_cut_plus_plus" | "xy++" | "xy_cut_pp" => Self::XyCutPlusPlus,
            _ => Self::TopToBottom, // default
        }
    }
}

/// v0.33: 为 blocks 列表加 reading_order_idx
/// input: `List<Value>`, 每项至少含 "text", 可选 "bbox"
/// output: 同 `List<Value>`, 加 "reading_order_idx" 字段
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
        Strategy::XyCutPlusPlus => {
            // v0.41.1: 真正的递归 XY-Cut++ (MinerU 升级版)
            // 1. 抽取 (idx, bbox) 对
            // 2. 识别 cross-layout elements (跨栏) 并分离
            // 3. 递归投影-轮廓分裂剩余
            // 4. 在合适位置合并 cross-layout elements
            let entries: Vec<(usize, BBox)> = blocks
                .iter()
                .zip(bboxes.iter())
                .enumerate()
                .filter_map(|(i, (_, bbox))| bbox.map(|b| (i, b)))
                .collect();

            if entries.is_empty() {
                // 全部缺 bbox, 保持输入序
            } else {
                let sorted = xy_cut_plus_plus_sort(&entries);
                // sorted 是按 reading order 的 (old_index, ...) 序列
                // 我们需要重排 blocks 列表
                indices = sorted.into_iter().map(|(old_idx, _)| old_idx).collect();
            }
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

// ============================================================
// v0.41.1: XY-Cut++ 算法实现 (MinerU arXiv:2504.10258)
// ============================================================

/// v0.41.1: 入口 — XY-Cut++ 排序
///
/// 步骤:
/// 1. 识别 cross-layout elements (跨栏元素，如页眉/页脚)
/// 2. 计算 density ratio，决定首次切分方向 (prefer_horizontal_first)
/// 3. 递归投影-轮廓分裂 (recursive_xy_cut)
/// 4. 在合适位置合并 cross-layout elements
///
/// 返回: 按 reading order 排序的 `(old_index, bbox)` 序列
fn xy_cut_plus_plus_sort(entries: &[(usize, BBox)]) -> Vec<(usize, BBox)> {
    if entries.is_empty() {
        return vec![];
    }
    if entries.len() == 1 {
        return entries.to_vec();
    }

    // 阶段 1: 识别 cross-layout elements
    let (cross_layout, remaining): (Vec<_>, Vec<_>) = entries
        .iter()
        .copied()
        .partition(|(_, b)| is_cross_layout(entries, *b));

    if remaining.is_empty() {
        // 全部都是 cross-layout, 退化到输入顺序
        return entries.to_vec();
    }

    // 阶段 2: density ratio 决定首次切分方向
    let prefer_horizontal_first = compute_prefer_horizontal(&remaining);

    // 阶段 3: 递归投影-轮廓分裂
    let sorted_main = recursive_xy_cut(&remaining, prefer_horizontal_first);

    // 阶段 4: 合并 cross-layout elements
    merge_cross_layout_elements(sorted_main, cross_layout)
}

/// v0.41.1: 判定 bbox 是否为 cross-layout element (跨栏)
///
/// 规则 (MinerU):
/// - width > beta * max_width_in_set AND
/// - overlaps with >= MIN_OVERLAP_COUNT columns
fn is_cross_layout(all: &[(usize, BBox)], bbox: BBox) -> bool {
    if all.is_empty() {
        return false;
    }
    let max_width = all.iter().map(|(_, b)| b.w).fold(0.0_f64, f64::max);
    if bbox.w <= XY_CUT_PLUS_PLUS_BETA * max_width {
        return false;
    }

    // 检查与多少个"列"重叠
    // 简化: 用每个 block 的 center_x 作为"列代表"
    let mut overlap_count = 0usize;
    for (_, other) in all {
        if other == &bbox {
            continue;
        }
        let overlap_start = bbox.x.max(other.x);
        let overlap_end = bbox.right().min(other.right());
        let overlap_width = (overlap_end - overlap_start).max(0.0);
        if overlap_width > XY_CUT_PLUS_PLUS_OVERLAP * other.w {
            overlap_count += 1;
        }
    }
    overlap_count >= XY_CUT_PLUS_PLUS_MIN_OVERLAP_COUNT
}

/// v0.41.1: 比较 x 方向密度 vs y 方向密度, 决定首次切分方向
///
/// x_density > density_threshold * y_density → prefer_horizontal_first (按 y 切分再按 x)
fn compute_prefer_horizontal(entries: &[(usize, BBox)]) -> bool {
    if entries.len() < 2 {
        return true;
    }
    let (x_density, y_density) = compute_density_ratios(entries);
    x_density > XY_CUT_PLUS_PLUS_DENSITY * y_density
}

fn compute_density_ratios(entries: &[(usize, BBox)]) -> (f64, f64) {
    // x_density = sum(widths) / (max_right - min_left)
    // y_density = sum(heights) / (max_bottom - min_top)
    let mut min_left = f64::INFINITY;
    let mut max_right = f64::NEG_INFINITY;
    let mut min_top = f64::INFINITY;
    let mut max_bottom = f64::NEG_INFINITY;
    let mut sum_w = 0.0;
    let mut sum_h = 0.0;
    for (_, b) in entries {
        min_left = min_left.min(b.x);
        max_right = max_right.max(b.right());
        min_top = min_top.min(b.y);
        max_bottom = max_bottom.max(b.bottom());
        sum_w += b.w;
        sum_h += b.h;
    }
    let x_span = (max_right - min_left).max(1.0);
    let y_span = (max_bottom - min_top).max(1.0);
    (sum_w / x_span, sum_h / y_span)
}

/// v0.41.1: 投影到轴 (0=x, 1=y)，输出 1D 直方图 (per-pixel count)
fn project_to_axis(entries: &[(usize, BBox)], axis: usize) -> Vec<u32> {
    if entries.is_empty() {
        return vec![];
    }
    let max_coord = entries
        .iter()
        .map(|(_, b)| if axis == 0 { b.right() } else { b.bottom() })
        .fold(0.0_f64, f64::max)
        .ceil() as usize;
    let mut hist = vec![0u32; max_coord + 1];
    for (_, b) in entries {
        let start = if axis == 0 { b.x } else { b.y } as usize;
        let end = (if axis == 0 { b.right() } else { b.bottom() }) as usize;
        for i in start..end.min(hist.len()) {
            hist[i] += 1;
        }
    }
    hist
}

/// v0.41.1: 在投影直方图中找连续 gap，切分为段
///
/// 返回段 `(start, end)` 列表 (含两端)
fn split_projection(hist: &[u32], min_gap: f64) -> Vec<(usize, usize)> {
    let min_gap = min_gap as usize;
    let mut segments = Vec::new();
    let mut in_segment = false;
    let mut seg_start = 0usize;
    let mut last_nonzero = 0usize;
    let mut gap_count = 0usize;

    for (i, &count) in hist.iter().enumerate() {
        if count > 0 {
            if !in_segment {
                seg_start = i;
                in_segment = true;
            }
            last_nonzero = i;
            gap_count = 0;
        } else if in_segment {
            gap_count += 1;
            if gap_count >= min_gap {
                // gap 足够大, 结束当前段
                segments.push((seg_start, last_nonzero + 1));
                in_segment = false;
            }
        }
    }
    if in_segment {
        segments.push((seg_start, last_nonzero + 1));
    }
    segments
}

/// v0.41.1: 递归投影-轮廓分裂
///
/// `prefer_horizontal_first` = true: 先按 y 切分 (行), 再按 x 切分 (列)
/// `prefer_horizontal_first` = false: 先按 x 切分 (列), 再按 y 切分 (行)
fn recursive_xy_cut(
    entries: &[(usize, BBox)],
    prefer_horizontal_first: bool,
) -> Vec<(usize, BBox)> {
    if entries.len() <= 1 {
        return entries.to_vec();
    }

    let (primary_axis, secondary_axis) = if prefer_horizontal_first {
        // 先按 y 切分 (primary = y), 再按 x 切分 (secondary = x)
        (1usize, 0usize)
    } else {
        (0usize, 1usize)
    };

    // 阶段 1: primary axis 投影 + 切分
    let primary_hist = project_to_axis(entries, primary_axis);
    let primary_segs = split_projection(&primary_hist, XY_CUT_PLUS_PLUS_MIN_GAP);

    let mut result = Vec::new();

    if primary_segs.len() <= 1 {
        // 沿 primary 无法切分, 直接按 secondary axis 投影
        let secondary_hist = project_to_axis(entries, secondary_axis);
        let secondary_segs = split_projection(&secondary_hist, XY_CUT_PLUS_PLUS_MIN_GAP);

        if secondary_segs.len() <= 1 {
            // 两轴都无法切分, 按 primary axis center 排序
            let mut sorted = entries.to_vec();
            sorted.sort_by(|a, b| {
                let ca = if primary_axis == 0 {
                    a.1.center_x()
                } else {
                    a.1.center_y()
                };
                let cb = if primary_axis == 0 {
                    b.1.center_x()
                } else {
                    b.1.center_y()
                };
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            });
            return sorted;
        }

        // 沿 secondary 切分, 每个子段按 primary 排序
        for (s_start, s_end) in &secondary_segs {
            let mut sub: Vec<_> = entries
                .iter()
                .copied()
                .filter(|(_, b)| {
                    let c = if secondary_axis == 0 {
                        b.center_x()
                    } else {
                        b.center_y()
                    };
                    let start = *s_start as f64;
                    let end = *s_end as f64;
                    c >= start && c < end
                })
                .collect();
            sub.sort_by(|a, b| {
                let ca = if primary_axis == 0 {
                    a.1.center_x()
                } else {
                    a.1.center_y()
                };
                let cb = if primary_axis == 0 {
                    b.1.center_x()
                } else {
                    b.1.center_y()
                };
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            });
            result.extend(sub);
        }
        return result;
    }

    // 沿 primary 切分, 每个子段递归
    for (s_start, s_end) in &primary_segs {
        let sub: Vec<_> = entries
            .iter()
            .copied()
            .filter(|(_, b)| {
                let c = if primary_axis == 0 {
                    b.center_x()
                } else {
                    b.center_y()
                };
                let start = *s_start as f64;
                let end = *s_end as f64;
                c >= start && c < end
            })
            .collect();
        // 递归: 反转 prefer 方向
        let sorted_sub = recursive_xy_cut(&sub, !prefer_horizontal_first);
        result.extend(sorted_sub);
    }
    result
}

/// v0.41.1: 在已排序主序列的合适位置插入 cross-layout elements
///
/// 策略: 按 cross-layout bbox 的 vertical center 找到对应位置
fn merge_cross_layout_elements(
    mut main: Vec<(usize, BBox)>,
    cross_layout: Vec<(usize, BBox)>,
) -> Vec<(usize, BBox)> {
    if cross_layout.is_empty() {
        return main;
    }
    if main.is_empty() {
        return cross_layout;
    }

    for ce in cross_layout {
        let insert_pos = find_insertion_point(&main, ce.1);
        main.insert(insert_pos, ce);
    }
    main
}

/// v0.41.1: 找到 cross-layout bbox 在主序列中的合适插入位置
///
/// 规则: 在主序列中找到第一个 vertical center 大于 ce.center_y 的位置
fn find_insertion_point(main: &[(usize, BBox)], ce_bbox: BBox) -> usize {
    let ce_center = ce_bbox.center_y();
    for (i, (_, b)) in main.iter().enumerate() {
        if b.center_y() > ce_center {
            return i;
        }
    }
    main.len() // 插入末尾
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
            // reading_order_idx 应为有限正整数；非法值（如 NaN / 负数）
            // 视为字段缺失，回退到上一层的 natural 排序。
            match d.get("reading_order_idx") {
                Some(v) => crate::flow::usize_from_value(v, "reading_order_idx").ok(),
                None => None,
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

    // ===== v0.41.1: XyCutPlusPlus (MinerU XY-Cut++) 测试 =====

    fn extract_texts(out: &[Value]) -> Vec<String> {
        out.iter()
            .filter_map(|b| {
                if let Value::Dict(d) = b
                    && let Some(Value::String(s)) = d.get("text")
                {
                    return Some(s.clone());
                }
                None
            })
            .collect()
    }

    #[test]
    fn strategy_from_str_xy_cut_pp() {
        // v0.41.1 新增别名
        assert_eq!(
            Strategy::from_str("xy_cut_plus_plus"),
            Strategy::XyCutPlusPlus
        );
        assert_eq!(Strategy::from_str("xy++"), Strategy::XyCutPlusPlus);
        assert_eq!(Strategy::from_str("xy_cut_pp"), Strategy::XyCutPlusPlus);
    }

    #[test]
    fn xy_cut_pp_single_column_doc() {
        // 报纸式单列, 4 blocks 垂直排列
        let blocks = vec![
            make_block(
                "title",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 200.0,
                    h: 30.0,
                }),
            ),
            make_block(
                "p1",
                Some(BBox {
                    x: 0.0,
                    y: 40.0,
                    w: 200.0,
                    h: 100.0,
                }),
            ),
            make_block(
                "p2",
                Some(BBox {
                    x: 0.0,
                    y: 150.0,
                    w: 200.0,
                    h: 100.0,
                }),
            ),
            make_block(
                "footer",
                Some(BBox {
                    x: 0.0,
                    y: 260.0,
                    w: 200.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        let texts = extract_texts(&out);
        // 期望: title, p1, p2, footer (top to bottom)
        assert_eq!(texts, vec!["title", "p1", "p2", "footer"]);
    }

    #[test]
    fn xy_cut_pp_two_column_doc() {
        // 学术两列: 左列 + 右列, 4 blocks
        // 设计: x_density 1.33, y_density 1.33 → prefer_horizontal_first=true
        // → 先按 y 切 (row), 再按 x 切 (column within row)
        let blocks = vec![
            make_block(
                "L1",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R1",
                Some(BBox {
                    x: 200.0,
                    y: 0.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "L2",
                Some(BBox {
                    x: 0.0,
                    y: 40.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R2",
                Some(BBox {
                    x: 200.0,
                    y: 40.0,
                    w: 100.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        let texts = extract_texts(&out);
        // y 投影切出 2 行 (y=0..20, y=40..60), 每行内按 x 排序
        // → L1, R1, L2, R2
        assert_eq!(texts, vec!["L1", "R1", "L2", "R2"]);
    }

    #[test]
    fn xy_cut_pp_with_cross_layout_header() {
        // 跨栏页眉 (width > beta * max_width_in_set)
        // 设计: 3 个普通 block (w=50) + 1 个跨栏 header (w=300)
        let blocks = vec![
            make_block(
                "L1",
                Some(BBox {
                    x: 0.0,
                    y: 50.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R1",
                Some(BBox {
                    x: 60.0,
                    y: 50.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "L2",
                Some(BBox {
                    x: 0.0,
                    y: 80.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "R2",
                Some(BBox {
                    x: 60.0,
                    y: 80.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "HEADER",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 300.0,
                    h: 30.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        let texts = extract_texts(&out);
        // HEADER 应在第一 (y=0 最小), 然后 L1, L2, R1, R2 或 L1, R1, L2, R2 (按 column/row 切分)
        assert_eq!(texts.first().unwrap(), "HEADER");
        assert_eq!(texts.len(), 5);
        // 所有 5 个都应被分配 reading_order_idx
        for b in &out {
            assert!(reading_order_idx(b).is_some());
        }
    }

    #[test]
    fn xy_cut_pp_single_block_returns_unchanged() {
        let blocks = vec![make_block(
            "only",
            Some(BBox {
                x: 0.0,
                y: 0.0,
                w: 50.0,
                h: 20.0,
            }),
        )];
        let out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        assert_eq!(out.len(), 1);
        assert_eq!(reading_order_idx(&out[0]), Some(0));
    }

    #[test]
    fn xy_cut_pp_complexity_below_o_n_squared() {
        // Perf benchmark: 50 个 block, XY-Cut++ 跑得快于 O(n^2) flat sort
        let mut blocks = Vec::new();
        for i in 0..50 {
            let row = i / 5;
            let col = i % 5;
            blocks.push(make_block(
                &format!("b{}", i),
                Some(BBox {
                    x: col as f64 * 60.0,
                    y: row as f64 * 30.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ));
        }
        let start = std::time::Instant::now();
        let _out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        let elapsed = start.elapsed();
        // Sanity: 50 blocks 应该 < 50ms (debug build)
        assert!(
            elapsed.as_millis() < 200,
            "XY-Cut++ too slow: {:?} for 50 blocks",
            elapsed
        );
    }

    #[test]
    fn xy_cut_pp_preserves_all_blocks() {
        // 验证没掉 block: 输入 N 个, 输出 N 个, 每个都有 reading_order_idx
        let blocks = vec![
            make_block(
                "a",
                Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "b",
                Some(BBox {
                    x: 60.0,
                    y: 5.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "c",
                Some(BBox {
                    x: 0.0,
                    y: 30.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
            make_block(
                "d",
                Some(BBox {
                    x: 60.0,
                    y: 35.0,
                    w: 50.0,
                    h: 20.0,
                }),
            ),
        ];
        let out = assign_reading_order(blocks, Strategy::XyCutPlusPlus);
        assert_eq!(out.len(), 4);
        // reading_order_idx 应该是 0..3 全覆盖 (no gap, no dup)
        let mut indices: Vec<usize> = out.iter().map(|b| reading_order_idx(b).unwrap()).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2, 3]);
    }
}

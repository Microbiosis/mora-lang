//! v0.75.59: XY-Cut++ 算法实现 — 自 reading_order/mod.rs 拆出（D6 单文件
//! 惯例）。MinerU arXiv:2504.10258 的递归投影-轮廓分裂 + cross-layout 处理。
//! 入口 xy_cut_plus_plus_sort 被 assign_reading_order 调用；共享类型 BBox
//! 在 super::mod。

use super::BBox;

/// v0.41.1: XY-Cut++ 算法常量
const XY_CUT_PLUS_PLUS_BETA: f64 = 2.0;
const XY_CUT_PLUS_PLUS_DENSITY: f64 = 0.9;
const XY_CUT_PLUS_PLUS_OVERLAP: f64 = 0.1;
const XY_CUT_PLUS_PLUS_MIN_OVERLAP_COUNT: usize = 2;
const XY_CUT_PLUS_PLUS_MIN_GAP: f64 = 5.0;

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
pub(super) fn xy_cut_plus_plus_sort(entries: &[(usize, BBox)]) -> Vec<(usize, BBox)> {
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

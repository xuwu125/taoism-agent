//! 云篆：符纸版式与笔画基元的**描述层**（docs/02 第 3.3 节）。
//!
//! 边界先说清楚，避免误会：
//!
//! - 这里给的是**几何描述**（整数坐标、归一化到 0..=1000），不是字形数据，也不是位图；
//! - 内置的六种「笔画基元」按**传统云篆造型规律**重绘（云卷 / 天柱 / 雷纹 / 波曲 /
//!   环钩 / 星点，见 [`STROKE_PRIMITIVES`] 注释），用来让 GUI 把版式与笔顺跑通；
//!   造型规律参考传统，**不冒充逐字考据的书法字帖**；真正的云篆字形需要字形数据源，
//!   属后续工作；
//! - 全部整数、无浮点，与项目「canonical 序列化无浮点」的约定一致；
//! - 本模块不做渲染，也不依赖任何 GUI 库——`fulu-ui` 只消费这些结构。
//!
//! 为什么先做描述层：T36（符纸热区 / 云篆笔顺）要的是「哪里画什么、什么顺序画」，
//! 这是纯数据问题，可以脱离渲染先定下来并测住。

use fulu_core::Talisman;

/// 符纸版式参数。全部是**千分比**，与画布实际像素无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudLayout {
    /// 画布宽高比（宽, 高）。符纸传统上是竖长方形。
    pub paper: (u32, u32),
    /// 符头（名号与法脉）占高度比例
    pub head_permille: u32,
    /// 符胆（正文）占高度比例
    pub body_permille: u32,
    /// 符脚（敕令与名号）占高度比例
    pub foot_permille: u32,
    /// 行高相对字高的千分比（1650 = 1.65 倍字高，留出符纸的行间气口）
    pub line_gap_permille: u32,
    /// 正文每行最多字数（超出则截断并标记，不做自动换行猜测）
    pub chars_per_line: u32,
    /// 正文最多行数
    pub max_lines: u32,
}

impl Default for CloudLayout {
    fn default() -> Self {
        // 三段相加必须正好 1000，有测试锁住。
        Self {
            paper: (3, 5),
            head_permille: 180,
            body_permille: 620,
            foot_permille: 200,
            line_gap_permille: 1650,
            chars_per_line: 14,
            max_lines: 18,
        }
    }
}

impl CloudLayout {
    pub fn height_total_permille(&self) -> u32 {
        self.head_permille + self.body_permille + self.foot_permille
    }
}

/// 符纸上的三个槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSlot {
    Head,
    Body,
    Foot,
}

impl CloudSlot {
    pub fn label(self) -> &'static str {
        match self {
            Self::Head => "符头",
            Self::Body => "符胆",
            Self::Foot => "符脚",
        }
    }
}

/// 一笔：名字 + 归一化折线（坐标都在 0..=1000）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke {
    pub name: &'static str,
    pub points: &'static [(i32, i32)],
}

/// 折线总长度（整数勾股，无浮点；与模块「全整数」的约定一致）。
pub fn polyline_length(points: &[(i32, i32)]) -> u64 {
    let mut total = 0u64;
    for pair in points.windows(2) {
        let dx = (pair[1].0 - pair[0].0) as i64;
        let dy = (pair[1].1 - pair[0].1) as i64;
        total += ((dx * dx + dy * dy) as u64).isqrt();
    }
    total
}

/// 把折线截到总长度的 `progress_permille`/1000 处，返回可见点列（末端按比例插值）。
///
/// - `0` → 只剩起点（长度为零，渲染层画不出任何线）；
/// - `1000` → 与原折线完全一致；
/// - 中间值 → 末端点落在折线上（逐段推进）。
pub fn partial_polyline(points: &[(i32, i32)], progress_permille: u32) -> Vec<(i32, i32)> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let total = polyline_length(points);
    if total == 0 {
        return vec![points[0]];
    }
    let target = total * u64::from(progress_permille.min(1000)) / 1000;
    let mut walked = 0u64;
    let mut visible = vec![points[0]];
    for pair in points.windows(2) {
        let dx = (pair[1].0 - pair[0].0) as i64;
        let dy = (pair[1].1 - pair[0].1) as i64;
        let segment = ((dx * dx + dy * dy) as u64).isqrt();
        if segment == 0 {
            continue; // 重合点：不推进也不插值。
        }
        if walked + segment >= target {
            // 末端落在这一段内：i64 全程线性插值，坐标仍落在 0..=1000 内。
            // remain == 0 时端点就是上一个顶点，不重复推。
            let remain = target - walked;
            if remain > 0 {
                let x = pair[0].0 as i64 + dx * remain as i64 / segment as i64;
                let y = pair[0].1 as i64 + dy * remain as i64 / segment as i64;
                visible.push((x as i32, y as i32));
            }
            return visible;
        }
        walked += segment;
        visible.push(pair[1]);
    }
    visible
}

/// 每笔至少 90 ms：太短的话「点」这种短笔一闪而过，人看不出笔顺。
const MIN_STROKE_MS: u64 = 90;

/// 笔顺演示的时长分配：每笔时长与其折线长度成正比，但不得低于 [`MIN_STROKE_MS`]。
///
/// 返回每笔的 `(start_ms, end_ms)`（前闭后开、首尾相接）。**末笔的 `end_ms`
/// 就是演示总时长**——因为保底时长可能把总长撑过请求值，调用方应以返回值为准，
/// 不自己假设「刚好等于 total_ms」。
pub fn stroke_schedule(strokes: &[Stroke], total_ms: u64) -> Vec<(u64, u64)> {
    let lengths: Vec<u64> = strokes.iter().map(|s| polyline_length(s.points)).collect();
    let sum: u64 = lengths.iter().sum();
    let mut schedule = Vec::with_capacity(strokes.len());
    let mut cursor = 0u64;
    for length in &lengths {
        let duration = (total_ms * length)
            .checked_div(sum)
            .unwrap_or(MIN_STROKE_MS)
            .max(MIN_STROKE_MS);
        schedule.push((cursor, cursor + duration));
        cursor += duration;
    }
    schedule
}

/// 六种笔画基元。**抽象几何，不是书法字形。**
/// 六种笔画基元：**按传统道教云篆的造型规律重绘**（2026-11-15，用户要求
/// 「参考传统道教的图形来处理」）。
///
/// - 「云卷」＝内卷螺旋，即符箓常见的卷云头 / 螺纹；
/// - 「天柱」＝竖钩蚓笔（S 形波动 + 底部出钩），符的骨干竖笔；
/// - 「雷纹」＝折电之形，符胆里常见的锯齿笔画；
/// - 「波曲」＝横波蚓笔，即符头下「三勾」式的波浪横笔；
/// - 「环钩」＝开环带尾钩，云篆的环形笔画；
/// - 「星点」＝短点，对应符面的星辰点缀。
///
/// **诚实边界**：这些是**几何描述**（整数坐标、归一化 0..=1000），造型规律
/// 参考了传统云篆（卷云 / 蚓笔 / 雷纹 / 环钩），但**不是逐字考据的真实
/// 符篆字形**——真字形仍需数据源（docs/02 §28.5），不冒充书法字帖。
pub const STROKE_PRIMITIVES: &[Stroke] = &[
    Stroke {
        name: "云卷",
        points: &[
            (520, 100),
            (667, 144),
            (782, 238),
            (849, 364),
            (861, 500),
            (821, 625),
            (740, 720),
            (633, 774),
            (520, 782),
            (418, 747),
            (342, 678),
            (301, 591),
            (297, 500),
            (328, 420),
            (383, 363),
            (452, 335),
            (520, 337),
            (577, 363),
            (615, 405),
            (630, 454),
            (624, 500),
            (603, 534),
            (573, 553),
            (543, 555),
        ],
    },
    Stroke {
        name: "天柱",
        points: &[
            (500, 80),
            (519, 131),
            (533, 182),
            (540, 232),
            (537, 283),
            (527, 334),
            (510, 385),
            (490, 435),
            (473, 486),
            (463, 537),
            (460, 588),
            (467, 638),
            (481, 689),
            (500, 740),
            (560, 770),
            (640, 830),
            (700, 790),
        ],
    },
    Stroke {
        name: "雷纹",
        points: &[
            (180, 150),
            (450, 300),
            (310, 430),
            (560, 570),
            (420, 700),
            (680, 850),
        ],
    },
    Stroke {
        name: "波曲",
        points: &[
            (120, 480),
            (152, 503),
            (183, 523),
            (215, 538),
            (247, 548),
            (278, 550),
            (310, 545),
            (342, 533),
            (373, 515),
            (405, 494),
            (437, 471),
            (468, 449),
            (500, 431),
            (532, 417),
            (563, 411),
            (595, 411),
            (627, 419),
            (658, 434),
            (690, 453),
            (722, 475),
            (753, 498),
            (785, 519),
            (817, 536),
            (848, 546),
            (880, 550),
        ],
    },
    Stroke {
        name: "环钩",
        points: &[
            (500, 220),
            (640, 258),
            (742, 360),
            (780, 500),
            (742, 640),
            (640, 742),
            (500, 780),
            (360, 742),
            (258, 640),
            (220, 500),
            (258, 360),
            (170, 420),
            (240, 330),
        ],
    },
    Stroke {
        name: "星点",
        points: &[(500, 420), (620, 560)],
    },
];

/// 一份可直接交给渲染层的描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudScript {
    pub layout: CloudLayout,
    /// 每个槽位的文本行（已按 `chars_per_line` / `max_lines` 裁剪）
    pub sections: Vec<(CloudSlot, Vec<String>)>,
    /// 正文是否被裁剪过（GUI 应据此显示提示，而不是静默丢字）
    pub truncated: bool,
    pub strokes: &'static [Stroke],
}

fn take_chars(text: &str, limit: u32) -> String {
    let limit = limit as usize;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_owned();
    }
    let mut clipped: String = chars.into_iter().take(limit).collect();
    clipped.push('…');
    clipped
}

/// 把符文映射到版式槽位。**只做映射与裁剪，不做排版算法**。
pub fn describe(fu: &Talisman) -> CloudScript {
    let layout = CloudLayout::default();
    let mut truncated = false;

    // 符头：符名 + 法脉（人一眼能认出「这是哪张符、请的哪一路」）。
    let head = vec![
        take_chars(&fu.name, layout.chars_per_line),
        take_chars(&fu.head.provider_ref, layout.chars_per_line),
    ];

    // 符胆：**按字符数折行**（中文按字折是确定的；不做断词猜测），再按总行数裁剪。
    let mut body: Vec<String> = Vec::new();
    for line in fu.system.split('\n') {
        if line.is_empty() {
            body.push(String::new());
            continue;
        }
        for chunk in line
            .chars()
            .collect::<Vec<char>>()
            .chunks(layout.chars_per_line as usize)
        {
            body.push(chunk.iter().collect());
        }
    }
    if body.len() as u32 > layout.max_lines {
        body.truncate(layout.max_lines as usize);
        // 末行还有位置就补一个省略号，明确「这里被截断了」。
        if let Some(last) = body.last_mut() {
            let mut chars: Vec<char> = last.chars().collect();
            if chars.len() as u32 == layout.chars_per_line {
                chars.pop();
            }
            chars.push('…');
            *last = chars.into_iter().collect();
        }
        truncated = true;
    }
    if body.is_empty() {
        // 空正文也要占位，否则三段比例在渲染时会塌掉。
        body.push(String::new());
    }

    // 符脚：敕令引用 + 神明名号。
    let foot = vec![
        take_chars(&fu.credential_ref, layout.chars_per_line),
        take_chars(&fu.deity.model_id, layout.chars_per_line),
    ];

    CloudScript {
        layout,
        sections: vec![
            (CloudSlot::Head, head),
            (CloudSlot::Body, body),
            (CloudSlot::Foot, foot),
        ],
        truncated,
        strokes: STROKE_PRIMITIVES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_segments_add_up_to_one_paper() {
        let layout = CloudLayout::default();
        assert_eq!(
            layout.height_total_permille(),
            1000,
            "三段比例必须正好铺满，否则渲染会留下未定义的缝"
        );
        assert!(layout.line_gap_permille > 1000, "行距应大于一个字高");
        assert!(layout.chars_per_line > 0 && layout.max_lines > 0);
    }

    #[test]
    fn length_and_partial_cut_follow_the_polyline() {
        let straight = [(120, 500), (880, 500)];
        assert_eq!(polyline_length(&straight), 760);
        assert_eq!(partial_polyline(&straight, 0), vec![(120, 500)]);
        assert_eq!(
            partial_polyline(&straight, 500),
            vec![(120, 500), (500, 500)]
        );
        assert_eq!(partial_polyline(&straight, 1000), straight.to_vec());
        assert_eq!(
            partial_polyline(&straight, 2000),
            straight.to_vec(),
            "超 1000 等同画完"
        );
        // 「点」只有两个点：截到一半也还是两个点，末端落在两点之间。
        let dot = &STROKE_PRIMITIVES[5];
        let half = partial_polyline(dot.points, 500);
        assert_eq!(half.len(), 2);
        assert!(half[1].0 > dot.points[0].0 && half[1].0 < dot.points[1].0);
    }

    #[test]
    fn partial_cut_walks_the_corner_not_the_chord() {
        // 「环钩」是 13 点的开环带尾钩：截到一半必须沿折线逐点走，不许直线切角。
        // 长度是唯一裁判：半程的部分折线长度 ≈ 总长的一半（坐标取整允许小误差）。
        let hook = &STROKE_PRIMITIVES[4];
        let half = partial_polyline(hook.points, 500);
        assert_eq!(half[0], hook.points[0], "起点不变");
        assert!(half.len() >= 3, "半程至少走到第三个点：{:?}", half);
        let total = polyline_length(hook.points);
        let walked = polyline_length(&half);
        let drift = (walked as i64 - total as i64 / 2).unsigned_abs();
        assert!(drift <= total / 20, "半程长度误差应 ≤5%：{drift} / {total}");
        // 完整走完必须原样返回。
        assert_eq!(partial_polyline(hook.points, 1000), hook.points.to_vec());
    }

    #[test]
    fn schedule_is_contiguous_and_respects_the_floor() {
        let schedule = stroke_schedule(STROKE_PRIMITIVES, 2400);
        assert_eq!(schedule.len(), STROKE_PRIMITIVES.len());
        assert_eq!(schedule[0].0, 0);
        for pair in schedule.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "笔与笔之间不许有空档");
        }
        for (start, end) in &schedule {
            assert!(end - start >= 90, "每笔至少 90 ms，否则看不出笔顺");
            assert!(start < end);
        }
        // 长笔（云卷螺旋 ~2100）分到的时长必须多于短笔（星点 ~193）。
        let juan = schedule[0].1 - schedule[0].0;
        let dian = schedule[5].1 - schedule[5].0;
        assert!(juan > dian, "时长应与折线长度成正比：{juan} vs {dian}");
        // 调用方应以末笔 end 为总时长，而不是请求值：比例时长向下取整、
        // 保底 90 ms 都可能让总长偏离请求值，只保证量级不变。
        let total = schedule.last().unwrap().1;
        assert!((2000..=3000).contains(&total), "总时长应接近 2400：{total}");
    }

    #[test]
    fn stroke_primitives_are_normalised_and_ordered() {
        assert_eq!(STROKE_PRIMITIVES.len(), 6);
        for stroke in STROKE_PRIMITIVES {
            assert!(stroke.points.len() >= 2, "`{}` 至少要两个点", stroke.name);
            for (x, y) in stroke.points {
                assert!((0..=1000).contains(x), "`{}` 的 x={x} 越界", stroke.name);
                assert!((0..=1000).contains(y), "`{}` 的 y={y} 越界", stroke.name);
            }
        }
        let names: Vec<&str> = STROKE_PRIMITIVES.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["云卷", "天柱", "雷纹", "波曲", "环钩", "星点"]);
    }

    #[test]
    fn cloud_strokes_follow_traditional_forms() {
        // 传统云篆造型规律的可测断言：云卷必须内卷（首末点距离远小于半径跨度）、
        // 天柱必须近似竖直（x 跨度小、y 跨度大）、雷纹必须折返（x 方向至少两次反向）、
        // 波曲必须横向波动（y 有起有伏）、环钩必须绕行（方位角覆盖 > 270°）。
        let juan = &STROKE_PRIMITIVES[0];
        // 卷心 (520,500)：首点在外圈（距心 > 300），末点必须卷回内圈（距心 < 150）。
        let dist = |p: (i32, i32)| -> u64 {
            let (dx, dy) = (p.0 as i64 - 520, p.1 as i64 - 500);
            (dx * dx + dy * dy) as u64
        };
        assert!(dist(juan.points[0]) > 90_000, "首点应在外圈");
        assert!(
            dist(*juan.points.last().unwrap()) < 22_500,
            "末点应卷回内圈"
        );

        let zhu = &STROKE_PRIMITIVES[1];
        let span_x = zhu
            .points
            .iter()
            .map(|p| p.0)
            .fold(i32::MAX, i32::min)
            .abs_diff(zhu.points.iter().map(|p| p.0).fold(i32::MIN, i32::max));
        let span_y = zhu
            .points
            .iter()
            .map(|p| p.1)
            .fold(i32::MAX, i32::min)
            .abs_diff(zhu.points.iter().map(|p| p.1).fold(i32::MIN, i32::max));
        assert!(span_y > span_x * 3, "天柱应以竖为主：{span_y} vs {span_x}");

        let lei = &STROKE_PRIMITIVES[2];
        let mut reversals = 0;
        for w in lei.points.windows(3) {
            let d1 = (w[1].0 - w[0].0).signum();
            let d2 = (w[2].1 - w[1].1).signum();
            let _ = d2;
            let dx2 = (w[2].0 - w[1].0).signum();
            if d1 != 0 && dx2 != 0 && d1 != dx2 {
                reversals += 1;
            }
        }
        assert!(reversals >= 3, "雷纹应至少三次折返：{reversals}");

        let bo = &STROKE_PRIMITIVES[3];
        let ys: Vec<i32> = bo.points.iter().map(|p| p.1).collect();
        let min_y = *ys.iter().min().unwrap();
        let max_y = *ys.iter().max().unwrap();
        assert!(max_y - min_y >= 100, "波曲应有明显起伏：{}", max_y - min_y);

        let huan = &STROKE_PRIMITIVES[4];
        // 绕行判定：环上相邻点的方位角增量同号且累计 > 270°。
        let cx = 500.0_f64;
        let cy = 500.0_f64;
        let mut total = 0.0_f64;
        let mut prev: Option<f64> = None;
        for p in huan.points.iter().take(11) {
            let ang = ((p.0 as f64 - cx).atan2(p.1 as f64 - cy)).to_degrees();
            if let Some(p0) = prev {
                let mut d = ang - p0;
                if d > 180.0 {
                    d -= 360.0;
                }
                if d < -180.0 {
                    d += 360.0;
                }
                total += d;
            }
            prev = Some(ang);
        }
        assert!(total.abs() > 270.0, "环钩应绕行超过 270°：{total}");
    }

    #[test]
    fn describe_fills_the_three_slots_in_order() {
        let script = describe(&Talisman::sample());
        let slots: Vec<CloudSlot> = script.sections.iter().map(|(slot, _)| *slot).collect();
        assert_eq!(
            slots,
            vec![CloudSlot::Head, CloudSlot::Body, CloudSlot::Foot]
        );
        let head = &script.sections[0].1;
        assert!(head[0].contains("镇宅"), "符头首行应是符名：{head:?}");
        assert!(head[1].starts_with("lu://"), "符头次行应是法脉：{head:?}");
        assert!(!script.truncated, "示例符不该被裁剪");
    }

    #[test]
    fn overlong_text_is_clipped_and_flagged_not_silently_dropped() {
        let mut fu = Talisman::sample();
        fu.system = "字".repeat(400);
        let script = describe(&fu);
        let body = &script.sections[1].1;
        assert!(script.truncated, "超长正文必须标记 truncated，供 GUI 提示");
        let longest = body.iter().map(|line| line.chars().count()).max().unwrap();
        assert!(longest <= 15, "每行应被裁到 chars_per_line 以内：{longest}");
    }

    #[test]
    fn many_lines_are_capped_at_max_lines() {
        let mut fu = Talisman::sample();
        fu.system = (0..100)
            .map(|i| format!("第{i}行"))
            .collect::<Vec<_>>()
            .join("\n");
        let script = describe(&fu);
        assert!(script.sections[1].1.len() <= CloudLayout::default().max_lines as usize);
        assert!(script.truncated);
    }

    #[test]
    fn empty_system_still_yields_one_placeholder_line() {
        let mut fu = Talisman::sample();
        fu.system = String::new();
        let script = describe(&fu);
        assert_eq!(script.sections[1].1.len(), 1, "空正文也要占一行");
        assert_eq!(script.sections[1].1[0], "");
        assert!(!script.truncated);
    }

    #[test]
    fn clipping_counts_characters_not_bytes() {
        // 中文一个字符三字节：按字节裁会切断 UTF-8，这里是按字符裁。
        let mut fu = Talisman::sample();
        fu.name = "符".repeat(20);
        let script = describe(&fu);
        let head = &script.sections[0].1[0];
        assert_eq!(head.chars().count(), 15, "14 字 + 一个省略号");
        assert!(head.ends_with('…'));
    }
}

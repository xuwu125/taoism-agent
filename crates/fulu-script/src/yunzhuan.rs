//! 云篆：符纸版式与笔画基元的**描述层**（docs/02 第 3.3 节）。
//!
//! 边界先说清楚，避免误会：
//!
//! - 这里给的是**几何描述**（整数坐标、归一化到 0..=1000），不是字形数据，也不是位图；
//! - 内置的六种「笔画基元」是**抽象折线**（横、竖、撇、捺、折、点），用来让 GUI 先把版式
//!   与笔顺跑通，**不冒充书法字帖**；真正的云篆字形需要字形数据源，属后续工作；
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

/// 六种笔画基元。**抽象几何，不是书法字形。**
pub const STROKE_PRIMITIVES: &[Stroke] = &[
    Stroke {
        name: "横",
        points: &[(120, 500), (880, 500)],
    },
    Stroke {
        name: "竖",
        points: &[(500, 120), (500, 880)],
    },
    Stroke {
        name: "撇",
        points: &[(760, 160), (240, 840)],
    },
    Stroke {
        name: "捺",
        points: &[(240, 160), (760, 840)],
    },
    Stroke {
        name: "折",
        points: &[(220, 200), (780, 200), (780, 780)],
    },
    Stroke {
        name: "点",
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
        assert_eq!(names, vec!["横", "竖", "撇", "捺", "折", "点"]);
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

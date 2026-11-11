//! 符纸视图（T36）：把一张符映射成**竖向条带**，每条带一个可点选热区。
//!
//! 边界先说清楚：
//!
//! - 这里只算**版式与热区的数据**（归一化到 `0.0..=1.0` 的纵坐标），不碰 `egui::Painter`；
//!   渲染在 `main.rs` 里按这些数字画，方便脱离显示器做单元测试。
//! - 三段大比例（符头 / 符胆 / 符脚）来自 [`fulu_script::describe_cloud`] 的 `CloudLayout`；
//!   它只知道三段，而视频里的符有九个部件，所以**段内细分是本模块自己的事**，
//!   并用测试锁住「权重合计 = 1000、条带首尾相接、不重叠」。
//! - 「未成符」是数据判断，不是画法：某段必填字段为空，就把 `filled` 置 false，
//!   由渲染层画成灰底 + 虚线，绝不假装这张符是完整的。

use fulu_app::Fu;
use fulu_script::CloudScript;

/// 视频体系里的九个部件，顺序即符纸从上到下的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 符头：符名 + 法脉
    Head,
    /// 神明名号
    Deity,
    /// 敕令（凭据引用）
    Oath,
    /// 捆仙绳（预算 / 超时 / 沙箱 / 步数 / 网络）
    Harness,
    /// 天柱（系统指令）
    Sky,
    /// 地柱（用户输入）
    Earth,
    /// 风火轮（工具白名单）
    Wheel,
    /// 符胆（技能包引用）
    Dan,
    /// 符脚（所请事项 + 判据）
    Foot,
}

/// 九段的相对高度权重（合计必须 1000，有测试锁住）。
///
/// 取值的道理：符胆最大（正文最占地方），符头与捆仙绳次之（人一眼先看这两处），
/// 敕令与风火轮最窄（一行就够）。
const WEIGHTS: [(Band, u32); 9] = [
    (Band::Head, 130),
    (Band::Deity, 70),
    (Band::Oath, 60),
    (Band::Harness, 110),
    (Band::Sky, 100),
    (Band::Earth, 100),
    (Band::Wheel, 80),
    (Band::Dan, 270),
    (Band::Foot, 80),
];

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Self::Head => "符头",
            Self::Deity => "神明",
            Self::Oath => "敕令",
            Self::Harness => "捆仙绳",
            Self::Sky => "天柱",
            Self::Earth => "地柱",
            Self::Wheel => "风火轮",
            Self::Dan => "符胆",
            Self::Foot => "符脚",
        }
    }

    /// 这一段在符文里对应哪些字段（悬停提示用，也用于「去哪改」的指引）。
    pub fn fields(self) -> &'static str {
        match self {
            Self::Head => "name / head.provider_ref",
            Self::Deity => "deity.model_id / deity.sampling",
            Self::Oath => "credential_ref",
            Self::Harness => "harness.budget_tokens / timeout_secs / sandbox / max_steps / network",
            Self::Sky => "system（正文上段）",
            Self::Earth => "user（正文下段）",
            Self::Wheel => "tool_refs / harness.allow_tools",
            Self::Dan => "skill_refs（符胆：sha256 引用）",
            Self::Foot => "goal.description / goal.success_criteria / output",
        }
    }

    /// 这一段是否「已填」。留空的段落渲染成灰色虚线，**不假装成符**。
    fn is_filled(self, fu: &Fu) -> bool {
        match self {
            Self::Head => !fu.name.trim().is_empty() && !fu.head.provider_ref.trim().is_empty(),
            Self::Deity => !fu.deity.model_id.trim().is_empty(),
            Self::Oath => !fu.credential_ref.trim().is_empty(),
            Self::Harness => fu.harness.max_steps > 0 && fu.harness.budget_tokens > 0,
            Self::Sky => !fu.system.trim().is_empty(),
            Self::Earth => !fu.user.trim().is_empty(),
            Self::Wheel => !fu.tool_refs.is_empty(),
            Self::Dan => !fu.skill_refs.is_empty(),
            Self::Foot => {
                !fu.goal.description.trim().is_empty()
                    && !fu.goal.success_criteria.trim().is_empty()
            }
        }
    }
}

/// 一段条带：归一化纵坐标 `start..end`，加上该段要画的文字行。
#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub band: Band,
    pub start: f32,
    pub end: f32,
    pub filled: bool,
    /// 画在条带里的行；空向量表示「这里该有字，但一个都没有」。
    pub lines: Vec<String>,
}

/// 一份可直接交给渲染层的符纸版式。
#[derive(Debug, Clone, PartialEq)]
pub struct Paper {
    /// 画布宽高比（宽, 高），来自 `CloudLayout.paper`（传统竖长方）。
    pub aspect: (u32, u32),
    pub zones: Vec<Zone>,
    /// 正文字数超出版式时置 true，渲染层必须显示提示，不许静默截断。
    pub truncated: bool,
}

fn clip(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_owned();
    }
    let mut clipped: String = chars.into_iter().take(limit.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// 把符文映射成符纸版式。
///
/// 文字来源分两类：符胆直接用 [`CloudScript`] 已经折好行的正文；其余段落按各自字段
/// 取一行摘要（截断到 18 字），保证条带不会把字挤出画布——**宁可显示省略号，也不缩字**。
pub fn plan(fu: &Fu, script: &CloudScript) -> Paper {
    const LINE_LIMIT: usize = 18;

    let body_lines = script
        .sections
        .iter()
        .find(|(slot, _)| *slot == fulu_script::CloudSlot::Body)
        .map(|(_, lines)| lines.clone())
        .unwrap_or_default();

    let text_for = |band: Band| -> Vec<String> {
        match band {
            Band::Head => vec![
                clip(&fu.name, LINE_LIMIT),
                clip(&fu.head.provider_ref, LINE_LIMIT),
            ],
            Band::Deity => vec![clip(
                &format!(
                    "{} · T{:.1}",
                    fu.deity.model_id,
                    fu.deity.sampling.temperature_milli as f32 / 1000.0
                ),
                LINE_LIMIT,
            )],
            Band::Oath => vec![clip(&fu.credential_ref, LINE_LIMIT)],
            Band::Harness => vec![
                clip(
                    &format!(
                        "预算 {} tok · 时限 {}s",
                        fu.harness.budget_tokens, fu.harness.timeout_secs
                    ),
                    LINE_LIMIT,
                ),
                clip(
                    &format!(
                        "沙箱 {} · 步数 {} · 网络 {}",
                        sandbox_label(fu.harness.sandbox),
                        fu.harness.max_steps,
                        if fu.harness.network { "开" } else { "关" }
                    ),
                    LINE_LIMIT,
                ),
            ],
            Band::Sky => vec![clip(&one_line(&fu.system), LINE_LIMIT)],
            Band::Earth => vec![clip(&one_line(&fu.user), LINE_LIMIT)],
            Band::Wheel => {
                if fu.tool_refs.is_empty() {
                    Vec::new()
                } else {
                    vec![clip(&fu.tool_refs.join("、"), LINE_LIMIT)]
                }
            }
            Band::Dan => {
                if fu.skill_refs.is_empty() {
                    Vec::new()
                } else {
                    // 符胆是 `sha256:<hex>`，画全长没有意义：只留前 8 位，明确是引用。
                    let short: Vec<String> = fu
                        .skill_refs
                        .iter()
                        .map(|reference| clip(reference, 12))
                        .collect();
                    vec![clip(&short.join("、"), LINE_LIMIT)]
                }
            }
            Band::Foot => vec![
                clip(&fu.goal.description, LINE_LIMIT),
                clip(&format!("验 {}", fu.goal.success_criteria), LINE_LIMIT),
            ],
        }
    };

    let mut zones = Vec::with_capacity(WEIGHTS.len());
    let mut cursor = 0.0_f32;
    for (band, weight) in WEIGHTS {
        let start = cursor;
        let end = cursor + f32::from(weight as u16) / 1000.0;
        cursor = end;
        let lines = if band == Band::Dan {
            body_lines.clone()
        } else {
            text_for(band)
        };
        let filled = band.is_filled(fu) && !(band == Band::Dan && lines.is_empty());
        zones.push(Zone {
            band,
            start,
            end,
            filled,
            lines,
        });
    }
    // 浮点累加末位可能有误差，把最后一段收到 1.0，避免画布底部露一条缝。
    if let Some(last) = zones.last_mut() {
        last.end = 1.0;
    }

    Paper {
        aspect: script.layout.paper,
        zones,
        truncated: script.truncated,
    }
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn sandbox_label(sandbox: fulu_app::Sandbox) -> &'static str {
    match sandbox {
        fulu_app::Sandbox::Strict => "严格",
        fulu_app::Sandbox::Limited => "受限",
        fulu_app::Sandbox::Full => "开放",
    }
}

impl Paper {
    /// 命中测试：给定归一化纵坐标，返回所在条带。
    ///
    /// 左闭右开，最后一段的 `end` 也包含在内（否则点最底下一像素没反应）。
    pub fn band_at(&self, y: f32) -> Option<&Zone> {
        if !(0.0..=1.0).contains(&y) {
            return None;
        }
        self.zones
            .iter()
            .find(|zone| y >= zone.start && (y < zone.end || (zone.end >= 1.0 && y <= 1.0)))
    }

    /// 未成符的段落（渲染层据此提示「还差哪几处」）。
    pub fn missing(&self) -> Vec<Band> {
        self.zones
            .iter()
            .filter(|zone| !zone.filled)
            .map(|zone| zone.band)
            .collect()
    }
}

/// 把版式宽高比装进可用区域：先按宽度定，再按高度收，始终保持 `aspect` 不变。
///
/// 抽成纯函数是因为「窗口一小，符纸把编辑器和按钮挤出屏幕」是个很容易复发的坑，
/// 而它又完全可以用几个数字测住，不必占用显示器。
pub fn fit(aspect: (u32, u32), avail_w: f32, avail_h: f32) -> (f32, f32) {
    let ratio = aspect.1.max(1) as f32 / aspect.0.max(1) as f32;
    let width_limit = avail_w.clamp(120.0, 240.0);
    let height_limit = (avail_h * 0.55).max(180.0);
    let mut width = width_limit;
    let mut height = width * ratio;
    if height > height_limit {
        height = height_limit;
        width = height / ratio;
    }
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_app::Fu;

    fn sample() -> (Fu, CloudScript) {
        let fu = Fu::sample();
        let script = fulu_script::describe_cloud(&fu);
        (fu, script)
    }

    #[test]
    fn weights_sum_to_one_thousand() {
        let total: u32 = WEIGHTS.iter().map(|(_, weight)| weight).sum();
        assert_eq!(total, 1000, "九段权重必须正好铺满画布");
    }

    #[test]
    fn zones_are_contiguous_and_cover_the_whole_paper() {
        let (fu, script) = sample();
        let paper = plan(&fu, &script);
        assert_eq!(paper.zones.len(), 9);
        assert!((paper.zones[0].start - 0.0).abs() < f32::EPSILON);
        assert!((paper.zones.last().unwrap().end - 1.0).abs() < f32::EPSILON);
        for pair in paper.zones.windows(2) {
            assert!(
                (pair[0].end - pair[1].start).abs() < 1e-6,
                "{} 与 {} 之间出现缝隙或重叠",
                pair[0].band.label(),
                pair[1].band.label()
            );
        }
    }

    #[test]
    fn band_lookup_hits_the_right_zone_including_edges() {
        let (fu, script) = sample();
        let paper = plan(&fu, &script);
        assert_eq!(paper.band_at(0.0).unwrap().band, Band::Head);
        assert_eq!(paper.band_at(1.0).unwrap().band, Band::Foot);
        assert!(paper.band_at(1.2).is_none(), "越界不该命中任何段");
        assert!(paper.band_at(-0.1).is_none());
        for zone in &paper.zones {
            let middle = (zone.start + zone.end) / 2.0;
            assert_eq!(paper.band_at(middle).unwrap().band, zone.band);
        }
    }

    #[test]
    fn empty_bands_report_missing_instead_of_pretending() {
        let (mut fu, _) = sample();
        fu.tool_refs.clear();
        fu.skill_refs.clear();
        fu.user = "   ".to_owned();
        let script = fulu_script::describe_cloud(&fu);
        let paper = plan(&fu, &script);
        let missing = paper.missing();
        assert!(missing.contains(&Band::Wheel), "空工具白名单必须报缺");
        assert!(missing.contains(&Band::Dan), "空符胆必须报缺");
        assert!(missing.contains(&Band::Earth), "空白地柱必须报缺");
    }

    #[test]
    fn body_lines_come_from_the_cloud_script_not_from_a_second_wrapping() {
        let (mut fu, _) = sample();
        fu.system = "字".repeat(400);
        let script = fulu_script::describe_cloud(&fu);
        let paper = plan(&fu, &script);
        let dan = paper.zones.iter().find(|z| z.band == Band::Dan).unwrap();
        assert_eq!(
            dan.lines.len(),
            fulu_script::CloudLayout::default().max_lines as usize
        );
        assert!(script.truncated);
        assert!(paper.truncated, "截断标记必须透传，方便渲染层提示");
        // 折行宽度与版式参数一致：每行不超过 chars_per_line（末行可能含省略号）。
        let limit = fulu_script::CloudLayout::default().chars_per_line as usize;
        for line in &dan.lines {
            assert!(line.chars().count() <= limit, "行超宽：{line}");
        }
    }

    #[test]
    fn long_field_text_is_clipped_so_bullets_never_overflow() {
        let (mut fu, _) = sample();
        fu.goal.description = "很长的所求".repeat(20);
        let script = fulu_script::describe_cloud(&fu);
        let paper = plan(&fu, &script);
        let foot = paper.zones.iter().find(|z| z.band == Band::Foot).unwrap();
        for line in &foot.lines {
            assert!(line.chars().count() <= 18, "行超宽：{line}");
        }
        assert!(foot.lines[0].ends_with('…'), "截断必须可见");
    }

    #[test]
    fn paper_fit_keeps_the_aspect_and_never_overflows() {
        // 宽窗：宽度说了算。
        let (w, h) = fit((3, 5), 400.0, 900.0);
        assert!((w - 240.0).abs() < 0.01, "宽度上限 240");
        assert!((h / w - 5.0 / 3.0).abs() < 0.001, "宽高比必须保持 3:5");

        // 矮窗：高度说了算，宽度跟着缩。
        let (w2, h2) = fit((3, 5), 400.0, 300.0);
        assert!((h2 - 180.0).abs() < 0.01, "高度下限 180");
        assert!((h2 / w2 - 5.0 / 3.0).abs() < 0.001);

        // 极窄窗：宽度下限 120，仍不塌成一条线。
        let (w3, _) = fit((3, 5), 20.0, 900.0);
        assert!((w3 - 120.0).abs() < 0.01);
    }

    #[test]
    fn aspect_comes_from_the_cloud_layout() {
        let (fu, script) = sample();
        let paper = plan(&fu, &script);
        assert_eq!(paper.aspect, fulu_script::CloudLayout::default().paper);
        assert_eq!(paper.aspect, (3, 5), "符纸是竖长方形");
    }
}

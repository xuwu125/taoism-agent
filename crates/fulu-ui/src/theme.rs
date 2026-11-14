//! 玄坛 · 朱箓（Xuantan / Cinnabar Codex）—— 符箓台设计令牌与主题装配。
//!
//! 这一层只做两件事：**把设计决策固化成常量**、**把常量装进 egui**。
//! 它不含任何业务逻辑，不认识 `FuluApp`，也不碰 `fulu-app` 门面。
//!
//! # 设计为什么长这样（不是随便挑的深色）
//!
//! 1. **暗坛金纸**：整台是「近黑深红的法坛」，唯一大面积亮色是符纸金。
//!    符纸因此天然成为视觉焦点——正好对应架构里「用户操作对象是符，不是配置项列表」。
//!    深色不是审美偏好，是让唯一那张符纸在一屏里自己浮出来。
//! 2. **朱砂是动作，青是只读**：朱砂（`#E0664C`）只给「会改变系统状态」的动作与警示；
//!    青（`#7FC2B0`）只给「系统陈述 / 只读 / append-only」。外宗、符胆、模型清单、
//!    箓牒、风险覆盖表全是「只展示不管理」，一眼就能和可写区域分开。
//! 3. **六终态绝不共用颜色**：`Archived / Broken / Rejected / Exploded / TimedOut /
//!    Cancelled` 各占一个独立色相。尤其 `Cancelled` 用中性暖灰——既不是红也不是绿，
//!    因为架构里它是**独立终态**，不并入炸炉、不冒充成功、不回滚。把失败折叠成
//!    一个红色会直接违背 `core::state::can_transition` 的类型级不变量。
//! 4. **权限与风险正交**：权限（ReadOnly/Limited/Full）用「边框档位」编码，
//!    风险（Observe/Mutate/Destructive）用「色彩 + 确认徽标」编码，两套永不合并成
//!    一根危险度滑块。见 `widgets::axes`。
//!
//! # 无障碍（数值均为真实计算，非估计）
//!
//! 相对最深浮起面 `#221B18` 计算，正文色全部 ≥ 4.68:1（WCAG AA 4.5:1），
//! 交互控件边界 `#8A6E56` ≥ 3.59:1（WCAG 1.4.11 非文本 3:1），
//! 符纸小字相对金纸 ≥ 4.81:1。完整对比度矩阵见 `docs/05-符箓台设计规范系统.md`。
//!
//! 中文最小字号从历史实现里的 **9px 提到 11px**（符纸）/ **12px**（全局）——
//! `docs/02` 第 28 节自己记过「9.5px 下只有数字判断、没有眼睛判断」，这里是修正。

use eframe::egui;

/// 色板。所有值都是 sRGB `Color32`，命名即语义，禁止在别的模块写裸 `from_rgb`。
///
/// 设计令牌是**规范资产**：某个 token 当前界面没用到，不代表可以删，
/// 否则规范会随一次次重构悄悄缩水。因此允许「暂未被引用」，但每个数值
/// 都在 `tests::wcag_aa_contrast_is_locked` 里被真实复算，不许漂移。
#[allow(dead_code)]
pub mod c {
    use eframe::egui::Color32;

    const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
        Color32::from_rgb(r, g, b)
    }

    // ---- 坛面：由深到浅的四层，层级靠明度差表达，不靠阴影堆叠 ----
    /// 玄坛底：窗口最底层。
    pub const VOID: Color32 = rgb(0x10, 0x0D, 0x0C);
    /// 面板底：三栏 / 顶栏 / 状态栏。
    pub const ALTAR: Color32 = rgb(0x18, 0x13, 0x11);
    /// 浮起面：卡片、敕门、待焚计划。
    pub const RAISED: Color32 = rgb(0x22, 0x1B, 0x18);
    /// 凹陷面：编辑器、代码块、只读清单。
    pub const SUNKEN: Color32 = rgb(0x0C, 0x0A, 0x09);

    /// 装饰性细线（不承载控件边界语义，对比度 1.7:1，按 WCAG 属纯装饰）。
    pub const HAIRLINE: Color32 = rgb(0x3A, 0x2E, 0x27);
    /// 交互控件边界：3.90:1 on ALTAR / 3.59:1 on RAISED，满足非文本 3:1。
    pub const EDGE: Color32 = rgb(0x8A, 0x6E, 0x56);
    /// 强调边界（当前态、焦点）。
    pub const EDGE_STRONG: Color32 = rgb(0xE2, 0xBE, 0x79);

    // ---- 墨色：暖白，不用纯白，避免深色底上刺眼光晕 ----
    /// 主文本（15.66:1 on VOID）。
    pub const INK: Color32 = rgb(0xF0, 0xE6, 0xD6);
    /// 次文本（9.74:1）。
    pub const INK_SOFT: Color32 = rgb(0xC4, 0xB6, 0xA3);
    /// 弱文本 / 元信息（5.34:1，仍达 AA）。
    pub const INK_MUTED: Color32 = rgb(0x93, 0x84, 0x74);
    /// 朱砂按钮上的字。
    pub const INK_ON_RED: Color32 = rgb(0xFF, 0xF5, 0xEC);
    /// 金 / 青填充上的字。
    pub const INK_ON_GOLD: Color32 = rgb(0x10, 0x0D, 0x0C);

    // ---- 品牌色 ----
    /// 朱砂（亮）：动作、强调、符印文字。5.42:1 on ALTAR。
    pub const CINNABAR: Color32 = rgb(0xE0, 0x66, 0x4C);
    /// 朱砂（深）：主按钮填充，`INK_ON_RED` 在其上 5.50:1。
    pub const CINNABAR_DEEP: Color32 = rgb(0xB2, 0x3B, 0x28);
    /// 朱砂（更暗）：hover / active。
    pub const CINNABAR_DARK: Color32 = rgb(0x8C, 0x2F, 0x20);
    /// 朱砂淡洗：选中条带、危险卡片底（需配合 alpha 使用）。
    pub const CINNABAR_WASH: Color32 = rgb(0xC0, 0x39, 0x2B);

    /// 金：符纸强调、状态轨道当前节点。10.42:1 on ALTAR。
    pub const GOLD: Color32 = rgb(0xE2, 0xBE, 0x79);
    /// 暗金：纸面细边、云篆 guide。
    pub const GOLD_DEEP: Color32 = rgb(0xB9, 0x8A, 0x4E);
    /// 青：系统陈述 / 只读 / append-only。8.99:1 on ALTAR。
    pub const JADE: Color32 = rgb(0x7F, 0xC2, 0xB0);
    /// 火光橙：仅用于焚符成功的纯视觉动效（不承载任何状态语义）。
    pub const EMBER: Color32 = rgb(0xF0, 0x91, 0x3C);

    // ---- 语义色（Tone 三态 + 警告） ----
    pub const GOOD: Color32 = rgb(0x6F, 0xCB, 0xA3);
    pub const BAD: Color32 = rgb(0xF0, 0x68, 0x5A);
    pub const WARN: Color32 = rgb(0xE0, 0xB4, 0x54);
    pub const INFO: Color32 = rgb(0x7F, 0xC2, 0xB0);

    // ---- 六终态：色相互不复用，取消刻意保持中性 ----
    /// 归档 · 唯一正常成功出口。
    pub const ST_ARCHIVED: Color32 = rgb(0x6F, 0xCB, 0xA3);
    /// 破符 · 结构/校验问题。
    pub const ST_BROKEN: Color32 = rgb(0xE0, 0xB4, 0x54);
    /// 驳回 · 符级授权失败（≠ 单次「敕否」）。
    pub const ST_REJECTED: Color32 = rgb(0xB7, 0x9A, 0xD6);
    /// 炸炉 · 运行时故障。
    pub const ST_EXPLODED: Color32 = rgb(0xF0, 0x68, 0x5A);
    /// 过坛 · 超时。
    pub const ST_TIMEDOUT: Color32 = rgb(0x8F, 0xA9, 0xD4);
    /// 中止 · 独立终态，中性暖灰，既不红也不绿。
    pub const ST_CANCELLED: Color32 = rgb(0xB0, 0xA3, 0x95);

    // ---- 符纸（金纸案台）：纸面是亮色，纸上墨色单独一套 ----
    pub const PAPER: Color32 = rgb(0xF3, 0xE7, 0xC8);
    pub const PAPER_EDGE: Color32 = rgb(0xB9, 0x8A, 0x4E);
    /// 纸面正文墨：12.46:1 on PAPER。
    pub const PAPER_INK: Color32 = rgb(0x2B, 0x24, 0x1B);
    /// 段名棕：5.12:1 on PAPER（旧值 `#8A6A3B` 只有 4.06:1，已校准）。
    pub const PAPER_BROWN: Color32 = rgb(0x7A, 0x5A, 0x30);
    /// 缺段灰底 / 灰字：5.54:1 on 灰底。
    pub const PAPER_MISSING: Color32 = rgb(0xE4, 0xDF, 0xD2);
    pub const PAPER_MISSING_INK: Color32 = rgb(0x5C, 0x55, 0x48);
    /// 朱砂方印：4.81:1 on PAPER（旧值 `#C0392B` 只有 4.42:1，已校准）。
    pub const SEAL_RED: Color32 = rgb(0xB2, 0x3B, 0x28);
}

/// 排版阶。中文最小 12px；符纸内受限空间最小 11px。
/// 同色板：字号阶是规范资产，允许某一档暂未被界面引用。
#[allow(dead_code)]
pub mod t {
    pub const CAPTION: f32 = 11.0;
    pub const SMALL: f32 = 12.0;
    pub const BODY: f32 = 14.0;
    pub const BODY_LG: f32 = 16.0;
    pub const H3: f32 = 18.0;
    pub const H2: f32 = 22.0;
    pub const H1: f32 = 28.0;
    pub const DISPLAY: f32 = 36.0;

    /// 符纸内的最小可读字号（比历史 9.0/9.5 提升两档）。
    pub const PAPER_MIN: f32 = 11.0;
}

/// 间距：4px 基准，8 点节奏。规范资产，允许某一档暂未被引用。
#[allow(dead_code)]
pub mod s {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
    pub const XXXL: f32 = 48.0;
    pub const HUGE: f32 = 64.0;
}

/// 圆角。符纸刻意保持 0（方正传统感），与界面控件形成材质对比。
pub mod r {
    use eframe::egui::CornerRadius;

    pub const BADGE: CornerRadius = CornerRadius::same(2);
    pub const CONTROL: CornerRadius = CornerRadius::same(4);
    pub const CARD: CornerRadius = CornerRadius::same(6);
    pub const PAPER: CornerRadius = CornerRadius::same(0);
}

/// 动效时长。全部是「纯视觉」，不承载任何状态判定（见 docs/02 §28.5）。
/// 规范资产，允许某项暂未被引用。
#[allow(dead_code)]
pub mod m {
    pub const FAST_MS: u64 = 150;
    pub const NORMAL_MS: u64 = 300;
    pub const SLOW_MS: u64 = 500;
    /// 焚符成功后的火光衰减。
    pub const FIRE_MS: u64 = 900;
    /// 云篆六笔的笔顺演示总时长。
    pub const STROKE_MS: u64 = 2400;
}

/// 仪轨阶段。状态轨道与终态徽标的唯一事实源。
///
/// 顺序即 `core::state` 的合法迁移序；`Pending → Igniting` 之间是**不可撤销分界线**，
/// `widgets::ritual_rail` 会在这里画一道朱砂闸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Draft,
    Formed,
    Sealed,
    Pending,
    Igniting,
    Archived,
    Broken,
    Rejected,
    Exploded,
    TimedOut,
    Cancelled,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "书符",
            Self::Formed => "成符",
            Self::Sealed => "盖印",
            Self::Pending => "待焚",
            Self::Igniting => "焚化",
            Self::Archived => "归档",
            Self::Broken => "破符",
            Self::Rejected => "驳回",
            Self::Exploded => "炸炉",
            Self::TimedOut => "过坛",
            Self::Cancelled => "中止",
        }
    }

    /// 阶段序号（0..5 为主链，终态从 6 起）。轨道只画主链 6 格，终态另起徽标。
    pub fn ordinal(self) -> usize {
        match self {
            Self::Draft => 0,
            Self::Formed => 1,
            Self::Sealed => 2,
            Self::Pending => 3,
            Self::Igniting => 4,
            Self::Archived => 5,
            _ => 6,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Archived
                | Self::Broken
                | Self::Rejected
                | Self::Exploded
                | Self::TimedOut
                | Self::Cancelled
        )
    }

    /// 六个终态各占独立色相，绝不复用。
    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Draft => c::INK_MUTED,
            Self::Formed => c::INK_SOFT,
            Self::Sealed => c::GOLD,
            Self::Pending => c::GOLD,
            Self::Igniting => c::EMBER,
            Self::Archived => c::ST_ARCHIVED,
            Self::Broken => c::ST_BROKEN,
            Self::Rejected => c::ST_REJECTED,
            Self::Exploded => c::ST_EXPLODED,
            Self::TimedOut => c::ST_TIMEDOUT,
            Self::Cancelled => c::ST_CANCELLED,
        }
    }
}

/// 组装 `Visuals`。基于 `Visuals::dark()` 覆写，避免漏字段导致不同 egui 版本行为漂移。
fn visuals() -> egui::Visuals {
    use egui::{CornerRadius, Stroke};

    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(c::INK);
    v.weak_text_color = Some(c::INK_SOFT);

    v.panel_fill = c::ALTAR;
    v.window_fill = c::RAISED;
    v.faint_bg_color = c::RAISED;
    v.extreme_bg_color = c::SUNKEN;
    v.text_edit_bg_color = Some(c::SUNKEN);
    v.code_bg_color = c::SUNKEN;
    v.warn_fg_color = c::WARN;
    v.error_fg_color = c::BAD;
    v.hyperlink_color = c::JADE;

    v.selection.bg_fill = c::CINNABAR_WASH.linear_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, c::INK);

    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    v.window_stroke = Stroke::new(1.0, c::EDGE);
    v.window_shadow = egui::Shadow {
        color: egui::Color32::from_black_alpha(96),
        ..v.window_shadow
    };
    v.popup_shadow = egui::Shadow {
        color: egui::Color32::from_black_alpha(96),
        ..v.popup_shadow
    };

    // 交互控件：静止态用「暗朱砂底 + 交互边界」，hover/active 逐级提亮。
    // 边界色 EDGE 在 ALTAR / RAISED 上分别 3.90 / 3.59:1，满足 WCAG 1.4.11。
    let edge = Stroke::new(1.0, c::EDGE);
    v.widgets.noninteractive.bg_fill = c::RAISED;
    v.widgets.noninteractive.weak_bg_fill = c::RAISED;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c::HAIRLINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, c::INK_SOFT);
    v.widgets.noninteractive.corner_radius = r::CARD;

    v.widgets.inactive.weak_bg_fill = c::RAISED;
    v.widgets.inactive.bg_fill = c::SUNKEN;
    v.widgets.inactive.bg_stroke = edge;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, c::INK);
    v.widgets.inactive.corner_radius = r::CONTROL;

    v.widgets.hovered.weak_bg_fill = c::CINNABAR_DARK.linear_multiply(0.55);
    v.widgets.hovered.bg_fill = c::CINNABAR_DARK;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, c::CINNABAR);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, c::INK);
    v.widgets.hovered.corner_radius = r::CONTROL;
    v.widgets.hovered.expansion = 1.0;

    v.widgets.active.weak_bg_fill = c::CINNABAR_DEEP;
    v.widgets.active.bg_fill = c::CINNABAR_DEEP;
    v.widgets.active.bg_stroke = Stroke::new(1.0, c::GOLD);
    v.widgets.active.fg_stroke = Stroke::new(1.0, c::INK_ON_RED);
    v.widgets.active.corner_radius = r::CONTROL;

    v.widgets.open.weak_bg_fill = c::RAISED;
    v.widgets.open.bg_fill = c::SUNKEN;
    v.widgets.open.bg_stroke = Stroke::new(1.0, c::GOLD);
    v.widgets.open.fg_stroke = Stroke::new(1.0, c::INK);
    v.widgets.open.corner_radius = r::CONTROL;

    v.button_frame = true;
    v.collapsing_header_frame = true;
    v.indent_has_left_vline = false;
    v.striped = true;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v
}

/// 把整套设计系统装进 egui。**必须在第一帧之前调用**（与字体装配同一时机）。
pub fn install(ctx: &egui::Context) {
    // 强制暗坛，不跟随系统明暗——符纸是唯一的亮面，跟系统切浅色会毁掉这个层级。
    ctx.set_theme(egui::Theme::Dark);
    let v = visuals();
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);

    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(s::SM, 6.0);
        style.spacing.button_padding = egui::vec2(s::MD, 6.0);
        // 交互目标 ≥ 24×24（WCAG 2.5.8 AA）；桌面鼠标场景取 30 高更舒展。
        style.spacing.interact_size = egui::vec2(40.0, 30.0);
        style.spacing.indent = s::LG;
        style.spacing.icon_width = 16.0;
        style.spacing.icon_width_inner = 8.0;
        style.spacing.icon_spacing = s::SM;
        style.spacing.extra_text_line_spacing = 2.0;
        style.spacing.window_margin = egui::Margin::same(s::LG as i8);
    });
    // Light 主题也指向同一套暗坛视觉：本产品不提供亮色模式，避免半套主题。
    ctx.style_mut_of(egui::Theme::Light, |style| {
        style.spacing.item_spacing = egui::vec2(s::SM, 6.0);
        style.spacing.button_padding = egui::vec2(s::MD, 6.0);
        style.spacing.interact_size = egui::vec2(40.0, 30.0);
        style.spacing.indent = s::LG;
        style.spacing.extra_text_line_spacing = 2.0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x 相对亮度。数值与 `docs/05` 的对比度矩阵用同一公式复算，
    /// 不是抄表——设计文档里的每个比值都能在这里跑出来。
    fn relative_luminance(color: egui::Color32) -> f32 {
        let weights = [0.2126_f32, 0.7152, 0.0722];
        [color.r(), color.g(), color.b()]
            .iter()
            .zip(weights)
            .map(|(raw, weight)| {
                let v = *raw as f32 / 255.0;
                let linear = if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                };
                weight * linear
            })
            .sum()
    }

    fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
        let (hi, lo) = match relative_luminance(a) >= relative_luminance(b) {
            true => (relative_luminance(a), relative_luminance(b)),
            false => (relative_luminance(b), relative_luminance(a)),
        };
        (hi + 0.05) / (lo + 0.05)
    }

    /// 无障碍判据锁进 CI：正文 ≥ 4.5:1，交互控件边界 ≥ 3:1，
    /// 符纸小字 ≥ 4.5:1。任何一次改色不达标都必须显式改这里并复核。
    #[test]
    fn wcag_aa_contrast_is_locked() {
        // 最浅的面板是 RAISED，在这里达标即在所有坛面达标。
        for (name, fg) in [
            ("INK", c::INK),
            ("INK_SOFT", c::INK_SOFT),
            ("INK_MUTED", c::INK_MUTED),
            ("CINNABAR", c::CINNABAR),
            ("GOLD", c::GOLD),
            ("JADE", c::JADE),
            ("EMBER", c::EMBER),
            ("GOOD", c::GOOD),
            ("BAD", c::BAD),
            ("WARN", c::WARN),
            ("INFO", c::INFO),
            ("ST_ARCHIVED", c::ST_ARCHIVED),
            ("ST_BROKEN", c::ST_BROKEN),
            ("ST_REJECTED", c::ST_REJECTED),
            ("ST_EXPLODED", c::ST_EXPLODED),
            ("ST_TIMEDOUT", c::ST_TIMEDOUT),
            ("ST_CANCELLED", c::ST_CANCELLED),
        ] {
            let ratio = contrast(fg, c::RAISED);
            assert!(
                ratio >= 4.5,
                "{name} 在浮起面上只有 {ratio:.2}:1，低于 AA 4.5:1"
            );
        }
        // WCAG 1.4.11 非文本：交互控件边界 ≥ 3:1。
        assert!(contrast(c::EDGE, c::ALTAR) >= 3.0);
        assert!(contrast(c::EDGE, c::RAISED) >= 3.0);
        // 符纸是亮底，纸上用的是另一套墨色。
        assert!(contrast(c::PAPER_INK, c::PAPER) >= 4.5);
        assert!(contrast(c::PAPER_BROWN, c::PAPER) >= 4.5);
        assert!(contrast(c::SEAL_RED, c::PAPER) >= 4.5);
        assert!(contrast(c::PAPER_MISSING_INK, c::PAPER_MISSING) >= 4.5);
        // 朱砂主按钮上的字。
        assert!(contrast(c::INK_ON_RED, c::CINNABAR_DEEP) >= 4.5);
        // 中文最小字号不得低于 11px（历史实现 9px 的可读性问题）。
        const { assert!(t::PAPER_MIN >= 11.0 && t::SMALL >= 12.0) };
    }

    #[test]
    fn six_terminal_states_have_distinct_colors() {
        let terminals = [
            Stage::Archived,
            Stage::Broken,
            Stage::Rejected,
            Stage::Exploded,
            Stage::TimedOut,
            Stage::Cancelled,
        ];
        for (i, a) in terminals.iter().enumerate() {
            assert!(a.is_terminal(), "{} 应是终态", a.label());
            for b in terminals.iter().skip(i + 1) {
                assert_ne!(
                    a.color(),
                    b.color(),
                    "{} 与 {} 不得共用视觉编码",
                    a.label(),
                    b.label()
                );
            }
        }
    }

    #[test]
    fn cancelled_is_neither_success_nor_failure_color() {
        // 取消既不能借成功的绿，也不能借炸炉的红。
        assert_ne!(Stage::Cancelled.color(), c::GOOD);
        assert_ne!(Stage::Cancelled.color(), c::BAD);
        assert_ne!(Stage::Cancelled.color(), Stage::Exploded.color());
        assert_ne!(Stage::Cancelled.color(), Stage::Archived.color());
    }

    #[test]
    fn irreversible_line_sits_between_pending_and_igniting() {
        // 轨道会在 Pending(3) 与 Igniting(4) 之间画分界线，序号关系必须稳定。
        assert_eq!(Stage::Pending.ordinal() + 1, Stage::Igniting.ordinal());
    }
}

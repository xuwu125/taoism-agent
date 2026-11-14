//! 组件库（纯绘制层）。
//!
//! 这里所有函数都只做「把语义画出来」，不认识 `FuluApp`、不碰 `fulu-app` 门面、
//! 不读取任何运行状态机。调用方负责把门面数据翻译成参数传进来——这样组件本身
//! 可以脱机单测，也不会把业务逻辑复制进 UI（架构铁律：GUI 只经门面）。
//!
//! 四类组件对应四条架构约束：
//! - `ritual_rail` / `terminal_badge` —— 状态机是一等视觉对象，六终态互不复用编码；
//! - `honesty_badge` —— 估算 / 未验证 / 截断 / 只读 / 纯展示 / 非结果 统一标注原子；
//! - `axes` —— 权限与风险**正交**的两套视觉编码，绝不合并成一根危险度滑块；
//! - `audit_row` —— 缴符 append-only 单向时间线，禁止任何可编辑/可删的视觉暗示。

use crate::theme::{Stage, c, r, s, t};
use eframe::egui::{self, Align2, Color32, RichText, Stroke};

/// 栏目头：左侧朱砂竖线 + 中文主标 + 小字副标（副标写清这一栏的「判断」而非装饰）。
pub fn section_title(ui: &mut egui::Ui, title: &str, subtitle: &str, accent: Color32) {
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, 1.0, accent);
        ui.add_space(s::XS);
        ui.label(RichText::new(title).size(t::H3).color(c::INK).strong());
        if !subtitle.is_empty() {
            ui.label(RichText::new(subtitle).size(t::SMALL).color(c::INK_MUTED));
        }
    });
}

/// 小徽标（只读展示，不可点）。用 Frame 而非自绘 galley，避免不同 egui 版本文字测量漂移。
pub fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(r::BADGE)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(t::CAPTION).color(fg));
        });
}

/// 诚实性标注原子。**项目价值观最密集的一层**：任何「不确定 / 不完整 / 非结论」
/// 都必须用同一种视觉语法说出来，不许各界面临场发挥。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Honesty {
    /// usage 是按字符数估算的，不是精确计数。
    Estimated,
    /// 清单 sha256 未固化，界面绝不替清单说好话。
    Unverified,
    /// 正文/分层被折叠或省略，绝不静默丢弃。
    Truncated,
    /// 该区域只读（外宗 / 符胆 / 模型清单 / 箓牒 / 风险覆盖表）。
    ReadOnly,
    /// 纯展示，不承载任何状态判定（火光 / 笔顺 / 太极层视图）。
    DisplayOnly,
    /// 流式增量不是回执，判定一律看回执。
    NotResult,
    /// 缴符写入前已脱敏。
    Redacted,
    /// 未安装确认器 → 一律敕否（fail closed）。
    FailClosed,
}

impl Honesty {
    pub fn label(self) -> &'static str {
        match self {
            Self::Estimated => "估算",
            Self::Unverified => "UNVERIFIED",
            Self::Truncated => "已截断",
            Self::ReadOnly => "只读 · 不管理",
            Self::DisplayOnly => "纯展示 · 不判定",
            Self::NotResult => "非回执",
            Self::Redacted => "已脱敏",
            Self::FailClosed => "无人值守 · 一律敕否",
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Self::Estimated | Self::Truncated => c::WARN,
            Self::Unverified | Self::FailClosed => c::BAD,
            Self::NotResult => c::EMBER,
            Self::ReadOnly | Self::DisplayOnly | Self::Redacted => c::JADE,
        }
    }
}

pub fn honesty_badge(ui: &mut egui::Ui, kind: Honesty) {
    let color = kind.color();
    chip(
        ui,
        kind.label(),
        color,
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32),
    );
}

/// 六终态徽标。颜色由 `Stage::color()` 唯一决定，调用方不得另行指定。
pub fn terminal_badge(ui: &mut egui::Ui, stage: Stage) {
    debug_assert!(stage.is_terminal(), "{} 不是终态", stage.label());
    let color = stage.color();
    chip(
        ui,
        stage.label(),
        color,
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30),
    );
}

/// 仪轨状态轨道。
///
/// 主链六格：书符 → 成符 → 盖印 → 待焚 → 焚化 → 归档。
/// **待焚与焚化之间画朱砂闸**：这是「不可撤销分界线」，对应架构里
/// `ignite(confirmed)` 之前必须先有 dry-run 计划且显式确认的硬红线。
/// 终态（破符/驳回/炸炉/过坛/中止）不挤进轨道，另起独立徽标，避免误读成主链一环。
pub fn ritual_rail(ui: &mut egui::Ui, current: Stage, terminal: Option<Stage>) {
    const MAIN: [Stage; 6] = [
        Stage::Draft,
        Stage::Formed,
        Stage::Sealed,
        Stage::Pending,
        Stage::Igniting,
        Stage::Archived,
    ];
    const H: f32 = 46.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, H), egui::Sense::hover());
    let p = ui.painter_at(rect);

    let n = MAIN.len() as f32;
    let pad = 16.0;
    let step = (rect.width() - 2.0 * pad) / (n - 1.0);
    let y = rect.top() + 15.0;

    // 已完成段用金线，未完成段用暗线；一眼看出走到哪一步。
    let cur = current.ordinal().min(5);
    for i in 0..(n as usize - 1) {
        let x0 = rect.left() + pad + i as f32 * step;
        let x1 = x0 + step;
        let done = i < cur;
        p.line_segment(
            [egui::pos2(x0 + 7.0, y), egui::pos2(x1 - 7.0, y)],
            Stroke::new(
                if done { 2.0 } else { 1.0 },
                if done { c::GOLD } else { c::HAIRLINE },
            ),
        );
    }

    // 不可撤销分界线：Pending(3) 与 Igniting(4) 之间，朱砂双竖线 + 小字。
    let gate_x = rect.left() + pad + 3.5 * step;
    for dx in [-2.0, 2.0] {
        p.line_segment(
            [
                egui::pos2(gate_x + dx, rect.top() + 2.0),
                egui::pos2(gate_x + dx, rect.top() + 30.0),
            ],
            Stroke::new(1.0, c::CINNABAR),
        );
    }
    p.text(
        egui::pos2(gate_x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        "不可撤销",
        egui::FontId::proportional(t::CAPTION),
        c::CINNABAR,
    );

    for (i, stage) in MAIN.iter().enumerate() {
        let x = rect.left() + pad + i as f32 * step;
        let reached = i <= cur;
        let is_current = current == *stage && terminal.is_none();
        let color = if is_current {
            c::EMBER
        } else if reached {
            c::GOLD
        } else {
            c::INK_MUTED
        };
        if is_current {
            p.circle_stroke(
                egui::pos2(x, y),
                9.0,
                Stroke::new(1.5, c::GOLD.linear_multiply(0.7)),
            );
        }
        p.circle_filled(egui::pos2(x, y), 5.0, color);
        p.text(
            egui::pos2(x, rect.top() + 26.0),
            Align2::CENTER_TOP,
            stage.label(),
            egui::FontId::proportional(t::SMALL),
            if is_current { c::INK } else { c::INK_SOFT },
        );
    }

    if let Some(end) = terminal {
        let color = end.color();
        let label = format!("终态 · {}", end.label());
        let bx = rect.right() - 6.0;
        p.text(
            egui::pos2(bx, rect.top() + 2.0),
            Align2::RIGHT_TOP,
            label,
            egui::FontId::proportional(t::SMALL),
            color,
        );
    }
}

/// 权限 / 风险正交双轴。
///
/// - 权限是**静态上限**（箓层）：用三格方块表达档位，形状编码，不用颜色深浅；
/// - 风险是**逐次确认强度**（敕门）：用色块 + 确认档位文字，颜色编码；
/// - 来源单独署小字（`外宗自述 / 覆盖表 / 未申报 / 策略下限`），回答「谁说的」。
///
/// 三者永不合并：把权限和风险画成一根滑块，会直接违反架构铁律。
pub struct Axes {
    pub permission: &'static str,
    pub permission_level: u8,
    pub risk: &'static str,
    pub risk_color: Color32,
    pub confirmation: &'static str,
    pub source: &'static str,
}

pub fn axes(ui: &mut egui::Ui, a: Axes) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("权限").size(t::SMALL).color(c::INK_MUTED));
        // 三格：填满的格数 = 权限档位；形状编码，与风险的颜色编码正交。
        for i in 0..3 {
            let (cell, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
            ui.painter().rect_filled(
                cell,
                1.0,
                if i < a.permission_level {
                    c::GOLD
                } else {
                    c::SUNKEN
                },
            );
            ui.painter().rect_stroke(
                cell,
                1.0,
                Stroke::new(1.0, c::EDGE),
                egui::StrokeKind::Inside,
            );
        }
        ui.label(RichText::new(a.permission).size(t::SMALL).color(c::INK));
        ui.add_space(s::MD);
        ui.label(RichText::new("风险").size(t::SMALL).color(c::INK_MUTED));
        chip(ui, a.risk, a.risk_color, c::SUNKEN);
        chip(ui, a.confirmation, c::INK_SOFT, c::RAISED);
        ui.add_space(s::MD);
        ui.label(
            RichText::new(format!("来源 · {}", a.source))
                .size(t::CAPTION)
                .color(c::INK_MUTED),
        );
    });
}

/// 卡片容器：浮起面 + 细边 + 左侧语义强调条。
pub fn card<R>(
    ui: &mut egui::Ui,
    accent: Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let frame = egui::Frame::new()
        .fill(c::RAISED)
        .stroke(Stroke::new(1.0, c::HAIRLINE))
        .corner_radius(r::CARD)
        .inner_margin(egui::Margin::same(s::MD as i8));
    let out = frame.show(ui, add_contents);
    let rect = out.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
        0.0,
        accent,
    );
    out.inner
}

/// 仪式按钮三档。`Ignite` 只在**计划存在 + 已勾选确认**时由调用方启用，
/// 组件本身不偷偷放宽条件——按钮的 enabled 状态就是红线的可视化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// 次要动作：扫描、清空、导出。
    Ghost,
    /// 仪轨动作：校验、盖印成符。
    Rite,
    /// 不可撤销动作：焚符。
    Ignite,
}

pub fn ritual_button(
    ui: &mut egui::Ui,
    label: &str,
    kind: ButtonKind,
    enabled: bool,
) -> egui::Response {
    let (fill, stroke, text_color) = match kind {
        ButtonKind::Ghost => (c::SUNKEN, Stroke::new(1.0, c::HAIRLINE), c::INK_SOFT),
        ButtonKind::Rite => (c::RAISED, Stroke::new(1.0, c::GOLD_DEEP), c::GOLD),
        ButtonKind::Ignite => (
            c::CINNABAR_DEEP,
            Stroke::new(1.0, c::CINNABAR),
            c::INK_ON_RED,
        ),
    };
    let button = egui::Button::new(
        RichText::new(label)
            .size(t::BODY)
            .strong()
            .color(if enabled { text_color } else { c::INK_MUTED }),
    )
    .fill(if enabled { fill } else { c::SUNKEN })
    .stroke(if enabled {
        stroke
    } else {
        Stroke::new(1.0, c::HAIRLINE)
    })
    .corner_radius(r::CONTROL);
    ui.add_enabled(enabled, button)
}

/// 只读区域标记。外宗、符胆、模型清单、箓牒、风险覆盖表、设置面板统一用它。
pub fn readonly_note(ui: &mut egui::Ui, write_hint: &str) {
    ui.horizontal_wrapped(|ui| {
        chip(ui, "只读 · 不管理", c::JADE, c::SUNKEN);
        if !write_hint.is_empty() {
            ui.label(
                RichText::new(write_hint)
                    .size(t::CAPTION)
                    .color(c::INK_MUTED),
            );
        }
    });
}

/// 缴符时间线的一行。左侧序号 + 链式节点，右侧等宽原文。
///
/// 刻意**没有任何按钮 / 拖拽柄 / 可选中框**：缴符是 append-only + sha256 链式摘要，
/// 界面不能给出「可以改 / 可以删 / 可以重排」的视觉暗示。
pub fn audit_row(ui: &mut egui::Ui, index: usize, line: &str, is_last: bool) {
    const DOT: f32 = 7.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let cx = rect.left() + DOT;
    let cy = rect.top() + 6.0;
    // 竖向链线：连到上一行、连到下一行，表达「链」而不是「列表」。
    p.line_segment(
        [egui::pos2(cx, rect.top()), egui::pos2(cx, cy - 3.0)],
        Stroke::new(1.0, c::JADE.linear_multiply(0.45)),
    );
    if !is_last {
        p.line_segment(
            [egui::pos2(cx, cy + 3.0), egui::pos2(cx, rect.bottom())],
            Stroke::new(1.0, c::JADE.linear_multiply(0.45)),
        );
    }
    p.circle_filled(egui::pos2(cx, cy), 2.5, c::JADE);
    p.text(
        egui::pos2(rect.left() + DOT * 2.0 + s::SM, rect.top()),
        Align2::LEFT_TOP,
        format!("{index:02}"),
        egui::FontId::monospace(t::CAPTION),
        c::INK_MUTED,
    );
    p.text(
        egui::pos2(rect.left() + DOT * 2.0 + s::SM + 22.0, rect.top()),
        Align2::LEFT_TOP,
        line,
        egui::FontId::monospace(t::SMALL),
        c::INK_SOFT,
    );
}

/// 符纸案台衬底：暗木色几层 + 纸影，让金纸在深色界面里「放上去」而不是「贴上去」。
/// 纯装饰，不承载任何状态。
pub fn paper_backdrop(p: &egui::Painter, rect: egui::Rect) {
    p.rect_filled(rect, r::CARD, c::SUNKEN);
    p.rect_stroke(
        rect,
        r::CARD,
        Stroke::new(1.0, c::HAIRLINE),
        egui::StrokeKind::Inside,
    );
    // 案台四角的暗金钉：装饰性的「装裱」语汇，不参与交互。
    let inset = 10.0;
    for (dx, dy) in [
        (inset, inset),
        (rect.width() - inset, inset),
        (inset, rect.height() - inset),
        (rect.width() - inset, rect.height() - inset),
    ] {
        p.circle_filled(
            egui::pos2(rect.left() + dx, rect.top() + dy),
            1.5,
            c::GOLD_DEEP.linear_multiply(0.6),
        );
    }
}

/// 分隔线（带语义色，区别于 egui 默认 separator）。
pub fn divider(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter_at(rect).rect_filled(rect, 0.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honesty_labels_are_all_distinct_and_nonempty() {
        let all = [
            Honesty::Estimated,
            Honesty::Unverified,
            Honesty::Truncated,
            Honesty::ReadOnly,
            Honesty::DisplayOnly,
            Honesty::NotResult,
            Honesty::Redacted,
            Honesty::FailClosed,
        ];
        let mut labels = std::collections::BTreeSet::new();
        for h in all {
            assert!(!h.label().is_empty());
            assert!(labels.insert(h.label()), "标注文案不得重复：{}", h.label());
        }
    }

    #[test]
    fn unverified_and_fail_closed_are_visually_urgent() {
        assert_eq!(Honesty::Unverified.color(), c::BAD);
        assert_eq!(Honesty::FailClosed.color(), c::BAD);
    }
}

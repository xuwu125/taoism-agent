//! 符纸写实绘制层（玄坛 · 朱箓 / 符文界面第二版）。
//!
//! 这一层**只画材质与纹样**：手工黄纸的纤维、水渍、折痕与毛边，朱砂的渗透与
//! 印泥斑驳，毛笔的起收笔与枯笔飞白，三清符头、云篆笔画列、符脚收笔与回纹边。
//! 它不认识 `FuluApp`、不读符文状态、不做任何判定，也不 import 领域 crate——
//! 调用方只把「画在哪、哪些笔画存在、有没有盖印」传进来。
//!
//! # 诚实性边界（不许越线）
//!
//! 符面上的云篆**是抽象笔画基元的组合，不是真实道教符篆字形**。真字形数据源
//! 至今仍缺（见 `docs/02` §28.5），所以：① 这里不生成任何可被当作「字」的
//! 书法字形；② 界面必须在纸外显式标注「示意笔画 · 非真符字形」；③ 缺段、
//! 截断、未盖印一律由界面如实标出，纸面不因为好看就假装完整或已盖印。
//! 火光、笔顺、纹样全是纯视觉，不承载任何状态判定。
//!
//! # 为什么纹理要缓存
//!
//! 纸纹是逐像素生成的，但 egui 每帧重绘。所有随机都来自**固定种子的确定性
//! 哈希**（`noise01`），纹理只生成一次由调用方缓存成 `TextureHandle`；
//! 否则既会拖慢帧率，也会让纸纹逐帧抖动闪烁。

use crate::theme;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke};

/// 固定种子确定性哈希，返回 `0.0..1.0`。同一坐标+种子永远同一个值。
fn noise01(x: i32, y: i32, seed: u32) -> f32 {
    let mut n = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ seed.wrapping_mul(0xC2B2_AE3D);
    n = n.wrapping_mul(0x27D4_EB2F);
    n ^= n >> 15;
    (n & 0xFFFF) as f32 / 65_535.0
}

/// 双线性插值的平滑噪声，用于水渍这类低频大块色差。
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let tx = x - x.floor();
    let ty = y - y.floor();
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let a = noise01(xi, yi, seed);
    let b = noise01(xi + 1, yi, seed);
    let c = noise01(xi, yi + 1, seed);
    let d = noise01(xi + 1, yi + 1, seed);
    (a + (b - a) * sx) + ((c + (d - c) * sx) - (a + (b - a) * sx)) * sy
}

/// 颜色线性插值。`from_rgb` 这里只承载**计算结果**，不写死任何色板值——
/// 所有源色都取自 `theme::c::*`，遵守「设计令牌是唯一事实源」。
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// 生成一张手工黄纸纹理（一次性，交给调用方缓存成 GPU 纹理）。
///
/// 三层叠加：① 中心亮、四周旧的底色渐变；② 竖向拉长的纸纤维 + 稀疏深色斑点；
/// ③ 三处低频水渍晕 + 两道折痕 + 四边不规则做旧。全部来自 `theme::c` 令牌，
/// 不引入任何外部素材，也不联网。
pub fn paper_texture(w: usize, h: usize) -> egui::ColorImage {
    let mut image = egui::ColorImage::new([w, h], vec![theme::c::PAPER; w * h]);
    let light = mix(theme::c::PAPER, Color32::WHITE, 0.14);
    let aged = mix(theme::c::PAPER, theme::c::PAPER_BROWN, 0.20);
    let burn = mix(theme::c::PAPER, theme::c::PAPER_BROWN, 0.46);
    let fw = w as f32;
    let fh = h as f32;

    for y in 0..h {
        let v = y as f32 / fh;
        for x in 0..w {
            let u = x as f32 / fw;
            let dx = (u - 0.5) * 2.0;
            let dy = (v - 0.5) * 2.0;
            let radial = (dx * dx + dy * dy).sqrt().min(1.0);

            // 1) 底色：手工纸中间受光、边缘自然泛旧。
            let mut col = mix(light, aged, radial * 0.62);

            // 2) 纤维：x 方向高频、y 方向低频 = 竖向拉长的纤维束。
            let fiber = value_noise(x as f32 / 2.4, y as f32 / 19.0, 0x51_7A) - 0.5;
            let grain = noise01(x as i32, y as i32, 0x7C_13) - 0.5;
            let shift = fiber * 0.055 + grain * 0.025;
            col = if shift >= 0.0 {
                mix(col, Color32::WHITE, shift)
            } else {
                mix(col, theme::c::PAPER_BROWN, -shift * 0.55)
            };

            // 3) 水渍：三处低频椭圆晕，边界用噪声轻微打散，不做规则圆。
            for (index, &(cx, cy, rx, ry)) in [
                (0.27_f32, 0.21_f32, 0.19_f32, 0.13_f32),
                (0.78_f32, 0.55_f32, 0.22_f32, 0.17_f32),
                (0.41_f32, 0.86_f32, 0.17_f32, 0.11_f32),
            ]
            .iter()
            .enumerate()
            {
                let nx = (u - cx) / rx;
                let ny = (v - cy) / ry;
                let jitter = value_noise(u * 9.0, v * 9.0, 0x31_00 + index as u32 * 17) - 0.5;
                let d = (nx * nx + ny * ny).sqrt() + jitter * 0.35;
                if d < 1.0 {
                    col = mix(col, aged, (1.0 - d) * 0.34);
                }
            }

            // 4) 折痕：一道竖、一道横，亮边 + 暗边成对，像纸被折过又展开。
            let crease_v = (u - 0.365).abs();
            let crease_h = (v - 0.655).abs();
            if crease_v < 0.004 {
                col = mix(col, Color32::WHITE, 0.30);
            } else if crease_v < 0.008 {
                col = mix(col, burn, 0.10);
            }
            if crease_h < 0.003 {
                col = mix(col, Color32::WHITE, 0.20);
            } else if crease_h < 0.006 {
                col = mix(col, burn, 0.08);
            }

            // 5) 稀疏深色斑点（纸料杂质），阈值固定、不会逐帧变。
            if noise01(x as i32, y as i32, 0xB0_41) > 0.9965 {
                col = mix(col, theme::c::PAPER_INK, 0.16);
            }

            // 6) 四边做旧：距边越近越深，边界用噪声打散成不规则「焦边」。
            let edge = (x.min(w - 1 - x).min(y.min(h - 1 - y))) as f32;
            let ragged = value_noise(x as f32 / 11.0, y as f32 / 11.0, 0x9D_02) * 7.0;
            if edge < 13.0 + ragged {
                let strength = (1.0 - edge / (13.0 + ragged)).clamp(0.0, 1.0);
                col = mix(col, burn, strength * 0.42);
            }

            image.pixels[y * w + x] = col;
        }
    }
    image
}

/// 暗木案台：木纹用矢量折线（每帧重绘几十条，比再传一张纹理便宜），
/// 纸下方加一层偏移软影表现「放上去」而不是「贴上去」。
pub fn wooden_desk(p: &Painter, rect: Rect, paper: Rect) {
    p.rect_filled(rect, 2.0, theme::c::SUNKEN);
    // 软影：三层逐渐变淡的偏移矩形，不用模糊滤镜也能给出厚度。
    for layer in 0..3_i32 {
        let d = 3.0 + layer as f32 * 3.0;
        let shadow = paper.translate(egui::vec2(d, d + 1.0));
        let opacity = 34_u8.saturating_sub((layer * 10) as u8);
        p.rect_filled(shadow, 0.0, alpha(Color32::BLACK, opacity));
    }
    // 木纹：横向长纤维，明暗交错，起止点带轻微波动。
    for row in 0..26 {
        let t = row as f32 / 26.0;
        let y = rect.top() + t * rect.height();
        let seed = 0x2A_00 + row as u32;
        let mut points = Vec::with_capacity(9);
        for step in 0..9 {
            let x = rect.left() + step as f32 / 8.0 * rect.width();
            let wobble = (noise01(step, row, seed) - 0.5) * 3.0;
            points.push(Pos2::new(x, y + wobble));
        }
        let bright = noise01(row, 0, 0x44_01) > 0.5;
        let color = if bright {
            alpha(theme::c::EDGE, 20)
        } else {
            alpha(theme::c::PAPER_INK, 34)
        };
        p.add(Shape::line(points, Stroke::new(0.7, color)));
    }
    p.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, theme::c::HAIRLINE),
        egui::StrokeKind::Inside,
    );
}

/// 纸的四边毛边：不规则短线，让边界不像 CSS 的 1px 直线。
pub fn deckle(p: &Painter, rect: Rect) {
    let ink = alpha(theme::c::PAPER_BROWN, 90);
    let step = 7.0;
    let mut x = rect.left() + 2.0;
    let mut index = 0_i32;
    while x < rect.right() - 2.0 {
        let len = 1.0 + noise01(index, 0, 0x1D_01) * 2.6;
        p.line_segment(
            [
                Pos2::new(x, rect.top()),
                Pos2::new(x + 1.0, rect.top() - len),
            ],
            Stroke::new(0.7, ink),
        );
        p.line_segment(
            [
                Pos2::new(x, rect.bottom()),
                Pos2::new(x + 1.0, rect.bottom() + len),
            ],
            Stroke::new(0.7, ink),
        );
        x += step;
        index += 1;
    }
    let mut y = rect.top() + 2.0;
    index = 0;
    while y < rect.bottom() - 2.0 {
        let len = 1.0 + noise01(0, index, 0x1D_02) * 2.6;
        p.line_segment(
            [
                Pos2::new(rect.left(), y),
                Pos2::new(rect.left() - len, y + 1.0),
            ],
            Stroke::new(0.7, ink),
        );
        p.line_segment(
            [
                Pos2::new(rect.right(), y),
                Pos2::new(rect.right() + len, y + 1.0),
            ],
            Stroke::new(0.7, ink),
        );
        y += step;
        index += 1;
    }
}

/// 内框回纹 + 四角星点：传统装裱纹样，纯装饰，不编码任何状态。
pub fn border_pattern(p: &Painter, rect: Rect) {
    let inner = rect.shrink(9.0);
    p.rect_stroke(
        inner,
        0.0,
        Stroke::new(0.7, alpha(theme::c::GOLD_DEEP, 120)),
        egui::StrokeKind::Inside,
    );
    let motif = alpha(theme::c::GOLD_DEEP, 92);
    let unit = 13.0;
    // 上下两边：连续小回纹（一个 L 形折线）。
    for side in [inner.top(), inner.bottom()] {
        let mut x = inner.left() + 6.0;
        while x < inner.right() - 12.0 {
            let dir = if side == inner.top() { 1.0 } else { -1.0 };
            p.add(Shape::line(
                vec![
                    Pos2::new(x, side),
                    Pos2::new(x + 7.0, side),
                    Pos2::new(x + 7.0, side + dir * 3.0),
                    Pos2::new(x + 2.0, side + dir * 3.0),
                ],
                Stroke::new(0.6, motif),
            ));
            x += unit;
        }
    }
    // 四角星点（三清点式的小三角点阵）。
    for (cx, cy) in [
        (inner.left() + 5.0, inner.top() + 5.0),
        (inner.right() - 5.0, inner.top() + 5.0),
        (inner.left() + 5.0, inner.bottom() - 5.0),
        (inner.right() - 5.0, inner.bottom() - 5.0),
    ] {
        for dx in [-2.0_f32, 0.0, 2.0] {
            p.circle_filled(Pos2::new(cx + dx, cy), 0.8, motif);
        }
    }
}

/// 三清符头：顶部三点、三道勾、两侧卷云，下方朱砂竖书「敕令」。
///
/// 注意：这是**传统符式的抽象描摹**，不是任何具体法脉的真实符头字样。
pub fn cloud_head(p: &Painter, rect: Rect) {
    let cinnabar = theme::c::SEAL_RED;
    let cx = rect.center().x;

    // 三清点。
    for dx in [-13.0_f32, 0.0, 13.0] {
        p.circle_filled(Pos2::new(cx + dx, rect.top() + 4.0), 2.1, cinnabar);
    }
    // 三道勾：每道是「起笔下压 → 右挑上扬」的折线，宽度递减，像连续书写的三勾。
    for (index, &scale) in [1.0_f32, 0.86, 0.72].iter().enumerate() {
        let y = rect.top() + 11.0 + index as f32 * 8.0;
        let half = 20.0 * scale;
        brush(
            p,
            &[
                Pos2::new(cx - half, y),
                Pos2::new(cx - half * 0.35, y + 5.0),
                Pos2::new(cx + half * 0.4, y + 1.0),
                Pos2::new(cx + half, y - 3.0),
            ],
            2.6 * scale,
            cinnabar,
            0x30_00 + index as u32,
        );
    }
    // 两侧卷云：对称的折线卷，抽象几何装饰。
    for side in [-1.0_f32, 1.0] {
        let base = cx + side * 30.0;
        let y = rect.top() + 22.0;
        p.add(Shape::line(
            vec![
                Pos2::new(base, y + 5.0),
                Pos2::new(base + side * 9.0, y + 5.0),
                Pos2::new(base + side * 13.0, y),
                Pos2::new(base + side * 9.0, y - 5.0),
                Pos2::new(base + side * 4.0, y - 4.0),
            ],
            Stroke::new(1.0, alpha(theme::c::GOLD_DEEP, 170)),
        ));
    }
    // 朱砂竖书「敕令」：通用传统程式语，不是伪造的符篆字形。
    let font = egui::FontId::proportional(theme::t::SMALL);
    p.text(
        Pos2::new(cx, rect.bottom() - 30.0),
        egui::Align2::CENTER_TOP,
        "敕",
        font.clone(),
        cinnabar,
    );
    p.text(
        Pos2::new(cx, rect.bottom() - 15.0),
        egui::Align2::CENTER_TOP,
        "令",
        font,
        cinnabar,
    );
}

/// 把归一化折线（0..=1000）映射到一个方框，并**曲线化**。
///
/// 描述层给的六笔是直线折线，直接画出来会像几何符号（实测像「米」字），离
/// 手写符篆太远。这里不新增任何笔画、不伪造字形，只是把每一段细分成若干点，
/// 沿法线加一条正弦主波 + 固定种子细抖，让同六个基元看起来是毛笔盘绕出来的。
/// 抖动完全确定性，逐帧不闪。
fn map_stroke(points: &[(i32, i32)], rect: Rect, seed: u32) -> Vec<Pos2> {
    let to_pixel = |(x, y): &(i32, i32)| {
        Pos2::new(
            rect.left() + (*x as f32 / 1000.0) * rect.width(),
            rect.top() + (*y as f32 / 1000.0) * rect.height(),
        )
    };
    if points.len() < 2 {
        return points.iter().map(to_pixel).collect();
    }
    let mut out: Vec<Pos2> = Vec::new();
    for (index, pair) in points.windows(2).enumerate() {
        let a = to_pixel(&pair[0]);
        let b = to_pixel(&pair[1]);
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let steps = ((len / 10.0).ceil() as usize).clamp(2, 8);
        let nx = -dy / len;
        let ny = dx / len;
        for step in 0..steps {
            let t = step as f32 / steps as f32;
            let wave = (t * std::f32::consts::PI).sin()
                * (1.3 + noise01(index as i32, step as i32, seed) * 1.5);
            let jitter = (noise01(index as i32, step as i32, seed ^ 0x5A5A) - 0.5) * 1.1;
            let off = wave + jitter;
            out.push(Pos2::new(a.x + dx * t + nx * off, a.y + dy * t + ny * off));
        }
    }
    if let Some(last) = points.last() {
        out.push(to_pixel(last));
    }
    out
}

/// 毛笔笔触：三层叠加（淡墨渗透晕 / 主墨 / 浓芯）+ 起收笔圆头 + 枯笔飞白。
/// 宽度沿笔画由重到轻（`taper` 用段序号算），不用 CSS 式的等宽线。
fn brush(p: &Painter, points: &[Pos2], width: f32, ink: Color32, seed: u32) {
    if points.len() < 2 {
        return;
    }
    // 渗透晕：更宽、更淡，模拟朱砂/墨在手工纸上的洇开。
    p.add(Shape::line(
        points.to_vec(),
        Stroke::new(width * 2.1, alpha(ink, 34)),
    ));
    p.add(Shape::line(
        points.to_vec(),
        Stroke::new(width * 1.35, alpha(ink, 90)),
    ));
    // 主墨：逐段收窄，起笔重收笔轻。
    for (index, pair) in points.windows(2).enumerate() {
        let t = index as f32 / (points.len() - 1) as f32;
        let w = (width * (1.18 - 0.46 * t)).max(0.6);
        p.line_segment([pair[0], pair[1]], Stroke::new(w, ink));
        p.circle_filled(pair[0], w * 0.5, ink);
    }
    if let Some(&last) = points.last() {
        p.circle_filled(last, width * 0.36, ink);
    }
    // 枯笔飞白：两三道极细纸色短线盖在墨上，位置由固定种子决定。
    for index in 0..2 {
        let total = points.len().saturating_sub(1);
        if total < 3 {
            break;
        }
        let at = 1 + (noise01(index, 0, seed) * (total - 2) as f32) as usize;
        let a = points[at.min(total - 1)];
        let b = points[(at + 1).min(total)];
        p.line_segment(
            [a, b],
            Stroke::new(width * 0.28, alpha(theme::c::PAPER, 210)),
        );
    }
}

/// 一组云篆笔画叠成的「符胆」复合符号。
///
/// **明确是抽象笔画基元，不是真符字形**：六笔来自 `fulu_script` 的描述层，
/// 这里只做映射与毛笔化。`drawn = false` 表示该段尚未成符，只画淡灰虚线框。
pub fn cloud_glyph(p: &Painter, rect: Rect, strokes: &[&[(i32, i32)]], drawn: bool, seed: u32) {
    if !drawn {
        // 缺段占位必须是**一整块**明示，不能只在顶部画一条虚线——符胆占纸高
        // 27%，只画顶边会留一大片空白，看起来像符写断了。淡灰底 + 四边虚线 +
        // 「未成符」小字，既填满版式节奏，又绝不冒充已经落墨的符。
        let guide = alpha(theme::c::PAPER_MISSING_INK, 150);
        p.rect_filled(rect, 0.0, alpha(theme::c::PAPER_MISSING, 105));
        let mut x = rect.left();
        while x < rect.right() {
            let end = (x + 5.0).min(rect.right());
            p.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(end, rect.top())],
                Stroke::new(1.0, guide),
            );
            p.line_segment(
                [Pos2::new(x, rect.bottom()), Pos2::new(end, rect.bottom())],
                Stroke::new(1.0, guide),
            );
            x += 9.0;
        }
        let mut y = rect.top();
        while y < rect.bottom() {
            let end = (y + 5.0).min(rect.bottom());
            p.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.left(), end)],
                Stroke::new(1.0, guide),
            );
            p.line_segment(
                [Pos2::new(rect.right(), y), Pos2::new(rect.right(), end)],
                Stroke::new(1.0, guide),
            );
            y += 9.0;
        }
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "未成符",
            egui::FontId::proportional(theme::t::PAPER_MIN),
            theme::c::PAPER_MISSING_INK,
        );
        return;
    }
    for (index, points) in strokes.iter().enumerate() {
        let mapped = map_stroke(points, rect, seed.wrapping_add(index as u32 * 131));
        brush(
            p,
            &mapped,
            (rect.width() * 0.032).clamp(1.4, 3.2),
            theme::c::PAPER_INK,
            seed.wrapping_add(index as u32 * 71),
        );
    }
}

/// 北斗式星点列：纸两侧的装饰点阵（纯装饰，不对应任何数据）。
pub fn star_dots(p: &Painter, rect: Rect) {
    let dot = alpha(theme::c::GOLD_DEEP, 135);
    let positions = [
        (0.12_f32, 0.10_f32),
        (0.22_f32, 0.22_f32),
        (0.15_f32, 0.38_f32),
        (0.26_f32, 0.52_f32),
        (0.13_f32, 0.70_f32),
        (0.24_f32, 0.84_f32),
    ];
    for (i, &(u, v)) in positions.iter().enumerate() {
        let radius = if i % 3 == 0 { 1.7 } else { 1.2 };
        p.circle_filled(
            Pos2::new(
                rect.left() + u * rect.width(),
                rect.top() + v * rect.height(),
            ),
            radius,
            dot,
        );
    }
}

/// 符脚：一笔到底的收势大勾 + 三点，传统符式里「收符」的抽象描摹。
pub fn talisman_foot(p: &Painter, rect: Rect) {
    let cx = rect.center().x;
    let ink = theme::c::PAPER_INK;
    brush(
        p,
        &[
            Pos2::new(cx - rect.width() * 0.32, rect.top() + 2.0),
            Pos2::new(cx - rect.width() * 0.10, rect.top() + rect.height() * 0.30),
            Pos2::new(cx + rect.width() * 0.12, rect.top() + rect.height() * 0.52),
            Pos2::new(cx + rect.width() * 0.30, rect.top() + rect.height() * 0.40),
            Pos2::new(cx + rect.width() * 0.34, rect.bottom() - 2.0),
        ],
        3.0,
        ink,
        0x7F_02,
    );
    for dx in [-8.0_f32, 0.0, 8.0] {
        p.circle_filled(
            Pos2::new(cx + dx, rect.bottom() - 4.0),
            1.5,
            theme::c::SEAL_RED,
        );
    }
}

/// 朱砂印泥方印：不规则边框 + 内部斑驳 + 印文 + 短摘要。
/// 只在真的盖过印时由调用方调用——**不盖印就绝不画印**。
pub fn seal_stamp(p: &Painter, rect: Rect, short_digest: &str) {
    let red = theme::c::SEAL_RED;
    // 印泥底：淡红打底，边缘略透。
    p.rect_filled(rect, 1.0, alpha(red, 28));
    // 边框：四条带轻微抖动的粗线，模拟印章压在纸上的不齐边缘。
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for index in 0..4 {
        let a = corners[index];
        let b = corners[(index + 1) % 4];
        let wobble = (noise01(index as i32, 0, 0x5A_01) - 0.5) * 1.6;
        p.line_segment(
            [Pos2::new(a.x, a.y + wobble), Pos2::new(b.x, b.y + wobble)],
            Stroke::new(2.4, alpha(red, 225)),
        );
    }
    p.rect_stroke(
        rect.shrink(3.0),
        0.0,
        Stroke::new(0.8, alpha(red, 190)),
        egui::StrokeKind::Inside,
    );
    // 斑驳：固定种子的缺口与浓点，避免印泥像一块纯色方块。
    for index in 0..22 {
        let u = noise01(index, 1, 0x5A_02);
        let v = noise01(index, 2, 0x5A_03);
        let pos = Pos2::new(
            rect.left() + u * rect.width(),
            rect.top() + v * rect.height(),
        );
        let radius = 0.5 + noise01(index, 3, 0x5A_04) * 1.4;
        p.circle_filled(pos, radius, alpha(theme::c::PAPER, 120));
    }
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "符",
        egui::FontId::proportional(rect.width() * 0.46),
        red,
    );
    if !short_digest.is_empty() {
        p.text(
            Pos2::new(rect.center().x, rect.bottom() + 1.0),
            egui::Align2::CENTER_TOP,
            short_digest,
            egui::FontId::monospace(8.0),
            alpha(red, 210),
        );
    }
}

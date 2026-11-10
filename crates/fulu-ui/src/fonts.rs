//! 中文字体装配。
//!
//! egui 自带的是 `epaint_default_fonts`：**Ubuntu 字体**（拉丁/西文的主流选择）+ NotoEmoji，
//! 但 Ubuntu 字体**一个汉字都没有**，egui 也不会自动回退到系统字体——直接用会把界面上的
//! 中文全画成方块（豆腐块）。所以汉字必须由宿主显式装配：
//!
//! 1. **内嵌思源黑体子集**（Noto Sans CJK SC，OFL-1.1，可随包分发）：截取 U+4E00–9FFF 全部
//!    汉字 + 常用标点/符号，6 MB 左右，编译进二进制。**零依赖用户机器**，任何系统开箱即用。
//! 2. **系统 CJK 字体兜底**：内嵌子集刻意不含扩展 A 区等生僻字，若真遇到，落在按平台候选
//!    列表找到的系统字体上（只读系统已有字体，不再分发）。
//!
//! 拉丁字母仍由 Ubuntu 字体负责（内嵌子集排在默认字体之后，只在缺字时被用到）。
//! 分发义务见 `THIRD_PARTY_NOTICES.md` 第 4 节。

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;

/// 内嵌的思源黑体子集（构建期定死，运行期不可能缺文件）。
const BUNDLED_SC_KEY: &str = "noto_sc_gui";
const BUNDLED_SC: &[u8] = include_bytes!("../assets/fonts/NotoSansSC-Regular-gui.otf");

/// 系统 CJK 字体在 `FontDefinitions` 里的键名。
const SYSTEM_KEY: &str = "system_cjk";

/// 各平台常见的 CJK 字体路径，按优先级排列，仅作内嵌子集缺字时的兜底。
///
/// `.ttc` 是字体集合文件，取第 0 号面（`FontData::from_owned` 的默认 index）即可，
/// 这几款字体的第 0 号面都是常规字重的简中面。
pub fn candidates() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // 优先微软雅黑；没有就退到黑体 / 宋体（老系统或精简镜像）。
        let windir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windir.join("Fonts");
        for name in ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"] {
            paths.push(fonts_dir.join(name));
        }
    }

    #[cfg(target_os = "macos")]
    {
        for path in [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ] {
            paths.push(PathBuf::from(path));
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        for path in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/arphic/uming.ttc",
        ] {
            paths.push(PathBuf::from(path));
        }
    }

    paths
}

/// 装配中文字体，返回给状态栏看的一句话。
///
/// 内嵌思源黑体子集必定成功；系统字体找到与否只影响生僻字兜底。
pub fn install_cjk_font(ctx: &egui::Context) -> String {
    let mut fonts = egui::FontDefinitions::default();

    // 1) 内嵌思源黑体子集：排在默认字体之后——拉丁字母仍用 Ubuntu 字体，
    //    内置字体没有的 CJK 码点落到这里。
    fonts.font_data.insert(
        BUNDLED_SC_KEY.to_owned(),
        Arc::new(egui::FontData::from_owned(BUNDLED_SC.to_vec())),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(BUNDLED_SC_KEY.to_owned());
    }

    // 2) 系统 CJK 字体兜底：排在最末，只有内嵌子集缺字（扩展 A 区等生僻字）时才会用到。
    let mut system_note = String::new();
    if let Some(path) = candidates().into_iter().find(|path| path.is_file()) {
        match std::fs::read(&path) {
            Ok(bytes) => {
                fonts.font_data.insert(
                    SYSTEM_KEY.to_owned(),
                    Arc::new(egui::FontData::from_owned(bytes)),
                );
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .push(SYSTEM_KEY.to_owned());
                }
                system_note = format!("；生僻字兜底：{}", path.display());
            }
            Err(error) => {
                system_note = format!("；系统字体 {} 读取失败（{error}）", path.display());
            }
        }
    }

    ctx.set_fonts(fonts);
    format!("中文字体：内嵌思源黑体子集{system_note}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_font_is_embedded_and_not_empty() {
        assert!(
            BUNDLED_SC.len() > 1_000_000,
            "内嵌思源黑体子集至少应有 1 MB，实际 {} 字节",
            BUNDLED_SC.len()
        );
        // OTF/CFF 头：「OTTO」。
        assert_eq!(&BUNDLED_SC[0..4], b"OTTO", "内嵌字体不是合法的 OTF");
    }

    #[test]
    fn candidates_are_absolute_and_unique() {
        let candidates = candidates();
        assert!(!candidates.is_empty(), "每个平台都必须至少有一个候选字体");
        for path in &candidates {
            assert!(
                path.is_absolute(),
                "候选路径必须是绝对路径：{}",
                path.display()
            );
        }

        let mut seen = std::collections::BTreeSet::new();
        for path in &candidates {
            assert!(
                seen.insert(path.clone()),
                "候选路径重复：{}",
                path.display()
            );
        }
    }

    #[test]
    fn extension_is_a_font_container() {
        for path in candidates() {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                matches!(extension.as_str(), "ttc" | "ttf" | "otf"),
                "只支持 ttc/ttf/otf：{}",
                path.display()
            );
        }
    }

    #[test]
    fn install_never_panics_and_reports_bundled_font() {
        // 无窗口环境下也要能跑：Context 不依赖系统窗口，找不到系统字体也只是少一层兜底。
        let ctx = egui::Context::default();
        let note = install_cjk_font(&ctx);
        assert!(
            note.contains("内嵌思源黑体子集"),
            "状态栏文案必须报告内嵌字体：{note}"
        );
        eprintln!("{note}");
    }

    /// 思源黑体子集必须覆盖界面上的关键汉字，缺一个就是界面上的方块。
    #[test]
    fn bundled_font_covers_key_ui_glyphs() {
        let ctx = egui::Context::default();
        install_cjk_font(&ctx);
        // egui 的字体表要到第一次 run 才初始化；无窗口跑一帧空 pass 即可，
        // 产出的纹理增量不交给渲染器，须显式 clear 才允许丢弃。
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            let supported = fonts.characters(&egui::FontFamily::Proportional);
            for ch in "符箓台法坛焚符缴符中文输入探针工作区模型清单权限确认，。（）".chars()
            {
                assert!(
                    supported.contains_key(&ch),
                    "缺字：U+{:04X} ({ch})",
                    ch as u32
                );
            }
        });
    }
}

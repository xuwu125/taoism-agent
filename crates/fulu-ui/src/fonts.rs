//! 中文字体装配。
//!
//! egui 自带的是 `epaint_default_fonts`（Ubuntu-Light + NotoEmoji），**没有任何 CJK 字形**，
//! 直接用会把界面上的中文全画成方块（豆腐块）。egui 也不会自动回退到系统字体，所以必须由
//! 宿主显式把一份系统 CJK 字体塞进 `FontDefinitions`。
//!
//! 取舍：
//! - **只读系统已有字体**，不随包分发字体文件（分发义务见 `THIRD_PARTY_NOTICES.md` 第 4 节）；
//! - 候选路径写死、不扫盘：启动开销固定，行为可预测，也避免在用户机器上乱翻文件；
//! - 加载失败不致命：退回内置字体，界面照常起，只是中文显示为方块，并在状态栏说明原因。

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;

/// 字体在 `FontDefinitions` 里的键名。
const FONT_KEY: &str = "system_cjk";

/// 各平台常见的 CJK 字体路径，按优先级排列。
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

/// 找到并安装一份系统 CJK 字体，返回给状态栏看的一句话。
pub fn install_cjk_font(ctx: &egui::Context) -> String {
    let Some(path) = candidates().into_iter().find(|path| path.is_file()) else {
        return "中文字体：未找到系统 CJK 字体，中文将显示为方块".to_owned();
    };

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return format!("中文字体：{} 读取失败（{error}）", path.display());
        }
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_KEY.to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    // 追加在默认字体之后：拉丁字母仍用 eframe 内置字体（字形更整齐），
    // 内置字体没有的 CJK 码点才落到系统字体上。
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(FONT_KEY.to_owned());
    }
    ctx.set_fonts(fonts);

    format!("中文字体：{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn install_never_panics_and_reports_a_result() {
        // 无窗口环境下也要能跑：Context 不依赖系统窗口，找不到字体也只是退回内置字体。
        let ctx = egui::Context::default();
        let note = install_cjk_font(&ctx);
        assert!(note.starts_with("中文字体："), "状态栏文案格式不符：{note}");
        eprintln!("{note}");
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
}

//! GUI 可替换性的**机器检查**（docs/02 第 29 节）。
//!
//! 「`fulu-ui` 与业务完全隔离，切换 iced 不影响领域层」这句话如果只写在文档里，
//! 半年后某个人在 `fulu-app` 里 `use eframe::egui` 就悄悄失效了。这里把它变成测试：
//!
//! - 除 `fulu-ui` 外，任何 crate 的 `Cargo.toml` 都不许出现 GUI 工具包；
//! - `fulu-ui` 必须仍然依赖当前主选（`eframe`），否则说明有人在没走决策流程的情况下换了栈。
//!
//! 只读清单、不读源码——依赖边界是清单层的契约，比源码 grep 更可靠。

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <workspace>/crates/fulu-app
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates 目录")
        .to_path_buf()
}

fn manifest_of(crate_name: &str) -> String {
    let path = workspace_root().join(crate_name).join("Cargo.toml");
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("读不到 {}：{error}", path.display());
    })
}

const FORBIDDEN: [&str; 5] = ["eframe", "egui", "iced", "winit", "slint"];

#[test]
fn only_the_ui_crate_depends_on_a_gui_toolkit() {
    let domain = [
        "fulu-core",
        "fulu-register",
        "fulu-provider",
        "fulu-runtime",
        "fulu-tools",
        "fulu-app",
        "fulu-audit",
        "fulu-model",
        "fulu-script",
        "fulu-mcp",
        "fulu-prompt",
        "fulu-context",
        "fulu-vault",
        "fulu-orchestrator",
        "fulu-skills",
        "fulu-cli",
    ];
    for name in domain {
        let manifest = manifest_of(name);
        for forbidden in FORBIDDEN {
            assert!(
                !manifest.contains(forbidden),
                "{name} 的 Cargo.toml 出现了 {forbidden}：领域层不得依赖 GUI 工具包，\
                 否则「换界面不动业务」就不成立了"
            );
        }
    }
}

#[test]
fn the_ui_crate_still_uses_the_agreed_toolkit() {
    let ui = manifest_of("fulu-ui");
    assert!(
        ui.contains("eframe"),
        "fulu-ui 不再依赖 eframe——若真换了栈，请先改 docs/03 第 8 节的决策记录"
    );
    // 备选栈不许「顺手」混进来：两套工具包同在一张清单里必然互相打架。
    assert!(!ui.contains("iced"), "fulu-ui 不应同时依赖 iced");
}

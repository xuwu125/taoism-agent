//! 内置本地基础模型：清单、校验、离线导入与缓存位置。
//!
//! 关键约束（docs/02 第 8.5 节）：
//! - 权重**不进 Git**，仓库只放 `models/manifest.json` 与合规文本；
//! - 未固化 sha256 时校验结果必须是 `UNVERIFIED`，不伪称通过；
//! - M0 不发起任何网络请求；`model fetch` 在 M2 落地。

pub mod manifest;
pub mod verify;

pub use manifest::{
    EMBEDDED_MANIFEST, ManifestError, ModelEntry, ModelManifest, ModelPolicy, ModelSampling,
};
pub use verify::{
    ImportReport, Verification, VerifyError, VerifyStatus, digest_file, import_file, verify_file,
};

use std::path::PathBuf;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn platform_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(home_dir)
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .map(|home| home.join("Library").join("Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".local").join("share")))
            .unwrap_or_else(std::env::temp_dir)
    }
}

/// 模型缓存根目录（不含权重本身）。
pub fn cache_dir() -> PathBuf {
    platform_data_dir().join("taoism-agent").join("models")
}

/// 某个槽位下权重应在的位置。
pub fn model_path(entry: &ModelEntry) -> PathBuf {
    cache_dir().join(&entry.slot).join(&entry.file)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    Missing { expected: PathBuf },
    Present { verification: Verification },
    Unreadable { path: PathBuf, error: String },
}

impl ModelStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Missing { .. } => "MISSING".into(),
            Self::Present { verification } => verification.status.label().into(),
            Self::Unreadable { .. } => "UNREADABLE".into(),
        }
    }
}

pub fn status(entry: &ModelEntry) -> ModelStatus {
    let path = model_path(entry);
    if !path.exists() {
        return ModelStatus::Missing { expected: path };
    }
    match verify_file(&path, entry) {
        Ok(verification) => ModelStatus::Present { verification },
        Err(error) => ModelStatus::Unreadable {
            path,
            error: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_is_namespaced() {
        let dir = cache_dir();
        let text = dir.to_string_lossy();
        assert!(text.contains("taoism-agent"), "{text}");
        assert!(text.ends_with("models"), "{text}");
    }

    #[test]
    fn missing_weight_is_reported_honestly() {
        let entry = ModelManifest::embedded()
            .unwrap()
            .default_entry()
            .unwrap()
            .clone();
        // 若本机恰好已导入权重，则只断言状态标签合法。
        match status(&entry) {
            ModelStatus::Missing { .. } => {}
            other => assert!(matches!(
                other.label().as_str(),
                "UNVERIFIED" | "VERIFIED" | "MISMATCH" | "UNREADABLE"
            )),
        }
    }
}

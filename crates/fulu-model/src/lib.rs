//! 内置本地基础模型：清单、校验、离线导入与缓存位置。
//!
//! 关键约束（docs/02 第 8.5 节）：
//! - 权重**不进 Git**，仓库只放 `models/manifest.json` 与合规文本；
//! - 未固化 sha256 时校验结果必须是 `UNVERIFIED`，不伪称通过；
//! - M0 不发起任何网络请求；`model fetch` 当前是**许可 + 校验闸门**，
//!   公网 HTTPS 下载与摘要固化仍列为 M2.1 待办（见 TODO.md T20/T21）。

pub mod digest_cache;
pub mod manifest;
pub mod seal;
pub mod verify;

pub use digest_cache::verify_file_cached;
pub use manifest::{
    EMBEDDED_MANIFEST, ManifestError, ModelEntry, ModelManifest, ModelPolicy, ModelSampling,
};
pub use seal::{SealError, SealProposal, apply_seal, metadata_url, parse_tree_metadata};
pub use verify::{
    ImportReport, Verification, VerifyError, VerifyStatus, digest_file, digest_file_state,
    import_file, verify_file,
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
    // 带 path+size+mtime 缓存：list / doctor / GUI 启动不再每次全量哈希 1.1 GB。
    // 显式校验命令 `model verify` 仍走 verify_file（全量，不走任何缓存）。
    match verify_file_cached(&path, entry, &digest_cache::cache_file()) {
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
        let mut entry = ModelManifest::embedded()
            .unwrap()
            .default_entry()
            .unwrap()
            .clone();
        // 固定指向一个不可能存在的文件：只验「缺权重如实报 Missing」，绝不
        // 去碰本机真实 1.1 GB 权重（那会让 CI 白跑一次全量 sha256）。
        entry.file = format!("fulu-test-absent-{}.gguf", std::process::id());
        match status(&entry) {
            ModelStatus::Missing { .. } => {}
            other => panic!("缺权重时应报 Missing，实际 {}", other.label()),
        }
    }
}

//! 内置模型清单（唯一事实源：`models/manifest.json`）。
//!
//! 「自带模型」= 自带**清单 + 校验 + 获取链路**，不是把 1.1 GB 权重塞进 Git。
//! 切换型号只需改清单，Rust 代码零改动。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 与 `models/manifest.json` 一起编译进二进制，保证离线也能列出模型信息。
pub const EMBEDDED_MANIFEST: &str = include_str!("../../../models/manifest.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPolicy {
    pub weights_in_git: bool,
    pub license_isolation: String,
    pub engine: String,
    pub bind: String,
    pub default_context_tokens: u32,
    pub fetch_requires_explicit_command: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSampling {
    pub temperature_milli: u32,
    pub top_k: u32,
    pub repeat_penalty_milli: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub slot: String,
    pub model_id: String,
    pub repo: String,
    pub file: String,
    pub source_url: String,
    pub license: String,
    pub license_spdx: String,
    pub license_url: String,
    pub quant: String,
    pub params: String,
    pub context_tokens: u32,
    pub default_context_tokens: u32,
    pub arch: String,
    pub approx_size: String,
    /// 精确字节数；未固化前为 None，**不得编造**
    pub bytes: Option<u64>,
    /// 真实 sha256；未固化前为 None，此时校验一律标记为 UNVERIFIED
    pub sha256: Option<String>,
    pub min_runtime: String,
    pub sampling: ModelSampling,
    pub runnable: bool,
    pub is_default: bool,
    pub notes: String,
}

impl ModelEntry {
    /// 摘要与体积都已登记，才可能得到 `Verified`。
    pub fn verification_ready(&self) -> bool {
        self.bytes.is_some() && self.sha256.as_ref().is_some_and(|hash| hash.len() == 64)
    }

    pub fn is_lfm_open_license(&self) -> bool {
        self.license == "LFM Open License v1.0" && self.license_spdx == "lfm1.0"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub updated: String,
    pub policy: ModelPolicy,
    pub entries: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Parse(String),
    NoDefault,
    MultipleDefaults,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "模型清单解析失败：{msg}"),
            Self::NoDefault => write!(f, "模型清单缺少默认槽位"),
            Self::MultipleDefaults => write!(f, "模型清单存在多个默认槽位"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl std::str::FromStr for ModelManifest {
    type Err = ManifestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))
    }
}

impl ModelManifest {
    pub fn load(path: &std::path::Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ManifestError::Parse(format!("{}: {e}", path.display())))?;
        text.parse()
    }

    /// 编译期嵌入的清单，供 CLI / GUI 在无网络时展示。
    pub fn embedded() -> Result<Self, ManifestError> {
        EMBEDDED_MANIFEST.parse()
    }

    pub fn entry(&self, slot: &str) -> Option<&ModelEntry> {
        self.entries.iter().find(|entry| entry.slot == slot)
    }

    pub fn default_entry(&self) -> Result<&ModelEntry, ManifestError> {
        let mut found = self.entries.iter().filter(|entry| entry.is_default);
        let first = found.next().ok_or(ManifestError::NoDefault)?;
        if found.next().is_some() {
            return Err(ManifestError::MultipleDefaults);
        }
        Ok(first)
    }

    /// 合规与完整性断言。CI 的 `check_model_manifest.py` 做同样的检查（双保险）。
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.schema_version != 1 {
            issues.push(format!("schema_version={} 不受支持", self.schema_version));
        }
        if self.policy.weights_in_git {
            issues.push("policy.weights_in_git 必须为 false：权重绝不入库".into());
        }
        if !self.policy.fetch_requires_explicit_command {
            issues.push("必须显式触发才能下载权重，禁止静默联网".into());
        }
        if !self.policy.engine.contains("llama-server") {
            issues.push("推理引擎应为外部 llama-server（OpenAI 兼容 HTTP）".into());
        }
        if self.policy.bind != "127.0.0.1" {
            issues.push("本地推理服务必须只绑定 127.0.0.1".into());
        }
        if self.default_entry().is_err() {
            issues.push("默认槽位必须恰好一个".into());
        }

        let mut slots = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let tag = format!("[{}]", entry.slot);
            if !slots.insert(entry.slot.as_str()) {
                issues.push(format!("{tag} 槽位重复"));
            }
            for (field, value) in [
                ("model_id", &entry.model_id),
                ("repo", &entry.repo),
                ("file", &entry.file),
                ("source_url", &entry.source_url),
                ("license", &entry.license),
                ("license_url", &entry.license_url),
                ("min_runtime", &entry.min_runtime),
            ] {
                if value.trim().is_empty() {
                    issues.push(format!("{tag} 缺少 {field}"));
                }
            }
            if !entry.is_lfm_open_license() {
                issues.push(format!("{tag} 许可标注不是 lfm1.0，须复核合规"));
            }
            if !entry.source_url.starts_with("https://") {
                issues.push(format!("{tag} source_url 必须使用 HTTPS"));
            }
            if entry.runnable && entry.default_context_tokens == 0 {
                issues.push(format!("{tag} 默认上下文必须 > 0"));
            }
            if !entry.runnable && entry.is_default {
                issues.push(format!("{tag} 不可运行的底座不能作为默认模型"));
            }
        }
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> ModelManifest {
        ModelManifest::embedded().expect("内嵌清单必须可解析")
    }

    #[test]
    fn embedded_manifest_passes_all_compliance_checks() {
        let issues = manifest().validate();
        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn exactly_one_default_and_it_is_runnable() {
        let m = manifest();
        let default = m.default_entry().unwrap();
        assert_eq!(default.slot, "default");
        assert!(default.runnable);
        assert_eq!(default.model_id, "lfm3-1.7b-q4km");
        assert!(default.is_lfm_open_license());
    }

    #[test]
    fn base_checkpoint_is_registered_but_not_runnable() {
        let m = manifest();
        let base = m.entry("finetune_base").unwrap();
        assert!(!base.runnable);
        assert_eq!(base.quant, "BF16");
    }

    #[test]
    fn no_fabricated_digests() {
        // 固化前守卫：所有精确字节数与摘要必须为空，而不是编造的值。
        // 注意：`model seal --confirm` 用 HF LFS 官方元数据固化后，本测试需要随之
        // 放宽为「每个已固化条目的值都必须来自 seal 的输出」——那时值不再是 null，
        // 但依然不许手写猜测值。
        for entry in manifest().entries {
            assert!(entry.bytes.is_none(), "{} 的字节数尚未固化", entry.slot);
            assert!(entry.sha256.is_none(), "{} 的 sha256 尚未固化", entry.slot);
            assert!(!entry.verification_ready());
        }
    }

    #[test]
    fn switching_default_is_a_data_only_change() {
        let mut m = manifest();
        for entry in &mut m.entries {
            entry.is_default = entry.slot == "fallback";
        }
        assert_eq!(m.default_entry().unwrap().slot, "fallback");
        assert!(m.validate().is_empty());
    }
}

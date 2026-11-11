//! 清单摘要固化（T20）：`model seal` 的纯函数核心。
//!
//! 摘要来源：Hugging Face tree API（`/api/models/<repo>/tree/main`）对 LFS 文件
//! 返回的 `lfs.oid`（sha256）与 `lfs.size`——这是**上游官方元数据**，不是猜测值。
//! 固化必须显式 `--confirm` 回写 `models/manifest.json`；固化前校验结果保持
//! `UNVERIFIED`，固化后 `model fetch` 才可能放行（不编造校验值的铁律不变）。

use crate::manifest::ModelManifest;
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealProposal {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    Metadata(String),
    NotFound(String),
    NotLfs(String),
    BadDigest(String),
    AlreadySealed {
        slot: String,
        existing: String,
        proposed: String,
    },
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metadata(msg) => write!(f, "元数据解析失败：{msg}"),
            Self::NotFound(msg) => write!(f, "元数据里找不到目标文件：{msg}"),
            Self::NotLfs(msg) => write!(f, "目标文件不是 LFS 对象，没有官方 sha256：{msg}"),
            Self::BadDigest(msg) => write!(f, "摘要不合法（拒绝固化）：{msg}"),
            Self::AlreadySealed {
                slot,
                existing,
                proposed,
            } => write!(
                f,
                "槽位 {slot} 已固化为 {existing}，与新提议 {proposed} 冲突；拒绝改写已固化摘要"
            ),
        }
    }
}

impl std::error::Error for SealError {}

/// HF tree API 元数据地址。repo 形如 `LiquidAI/LFM3-1.7B-GGUF`。
pub fn metadata_url(repo: &str) -> String {
    format!("https://huggingface.co/api/models/{repo}/tree/main")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 从 tree API 的 JSON 数组中找到 `file` 对应的 LFS 摘要与精确字节数。
pub fn parse_tree_metadata(text: &str, file: &str) -> Result<SealProposal, SealError> {
    let entries: Vec<Value> =
        serde_json::from_str(text).map_err(|e| SealError::Metadata(e.to_string()))?;
    let entry = entries
        .iter()
        .find(|entry| entry.get("path").and_then(Value::as_str) == Some(file))
        .ok_or_else(|| {
            SealError::NotFound(format!("{file}（tree API 返回 {} 个条目）", entries.len()))
        })?;

    let lfs = entry
        .get("lfs")
        .filter(|value| !value.is_null())
        .ok_or_else(|| SealError::NotLfs(format!("{file} 没有 lfs 元数据")))?;
    let oid = lfs
        .get("oid")
        .and_then(Value::as_str)
        .ok_or_else(|| SealError::NotLfs(format!("{file} 的 lfs.oid 缺失")))?;
    if !is_sha256_hex(oid) {
        return Err(SealError::BadDigest(format!(
            "lfs.oid 不是 64 位十六进制 sha256：{oid}"
        )));
    }
    let size = lfs
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| SealError::Metadata(format!("{file} 的 lfs.size 缺失或非法")))?;
    Ok(SealProposal {
        path: file.to_owned(),
        bytes: size,
        sha256: oid.to_ascii_lowercase(),
    })
}

/// 把提议写进清单（内存对象；落盘由调用方在 `--confirm` 后完成）。
/// 幂等：与已固化值完全一致时静默通过；冲突一律拒绝。
pub fn apply_seal(
    manifest: &mut ModelManifest,
    slot: &str,
    proposal: &SealProposal,
) -> Result<(), SealError> {
    let entry = manifest
        .entries
        .iter_mut()
        .find(|entry| entry.slot == slot)
        .ok_or_else(|| SealError::NotFound(format!("清单中没有槽位 {slot}")))?;
    if let (Some(existing_bytes), Some(existing_hash)) = (entry.bytes, entry.sha256.as_deref()) {
        if existing_bytes == proposal.bytes && existing_hash == proposal.sha256 {
            return Ok(());
        }
        return Err(SealError::AlreadySealed {
            slot: slot.to_owned(),
            existing: format!("{existing_hash}/{existing_bytes}"),
            proposed: format!("{}/{}", proposal.sha256, proposal.bytes),
        });
    }
    entry.bytes = Some(proposal.bytes);
    entry.sha256 = Some(proposal.sha256.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelManifest;

    fn sample_tree() -> String {
        r#"[
          {"type":"directory","path":".","oid":"abc"},
          {"type":"file","path":"README.md","size":1024,"lfs":null},
          {"type":"file","path":"LFM3-1.7B-Q4_K_M.gguf","size":1180000000,
           "lfs":{"oid":"A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2","size":1180000000,"pointerSize":170900000}},
          {"type":"file","path":"other.gguf","size":1,"lfs":{"oid":"zz","size":1}}
        ]"#
        .to_owned()
    }

    #[test]
    fn metadata_url_follows_repo() {
        assert_eq!(
            metadata_url("LiquidAI/LFM3-1.7B-GGUF"),
            "https://huggingface.co/api/models/LiquidAI/LFM3-1.7B-GGUF/tree/main"
        );
    }

    #[test]
    fn lfs_oid_and_size_are_extracted_and_lowercased() {
        let proposal = parse_tree_metadata(&sample_tree(), "LFM3-1.7B-Q4_K_M.gguf").unwrap();
        assert_eq!(proposal.bytes, 1_180_000_000);
        assert_eq!(
            proposal.sha256,
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2"
        );
    }

    #[test]
    fn missing_file_and_non_lfs_are_explicit() {
        assert!(matches!(
            parse_tree_metadata(&sample_tree(), "nope.gguf"),
            Err(SealError::NotFound(_))
        ));
        assert!(matches!(
            parse_tree_metadata(&sample_tree(), "README.md"),
            Err(SealError::NotLfs(_))
        ));
        assert!(matches!(
            parse_tree_metadata(&sample_tree(), "other.gguf"),
            Err(SealError::BadDigest(_))
        ));
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        assert!(matches!(
            parse_tree_metadata("not json", "x"),
            Err(SealError::Metadata(_))
        ));
        assert!(matches!(
            parse_tree_metadata("[]", "x"),
            Err(SealError::NotFound(_))
        ));
    }

    #[test]
    fn apply_seal_is_idempotent_and_conflict_resistant() {
        let mut manifest = ModelManifest::embedded().unwrap();
        let proposal = parse_tree_metadata(&sample_tree(), "LFM3-1.7B-Q4_K_M.gguf").unwrap();
        apply_seal(&mut manifest, "default", &proposal).unwrap();
        let entry = manifest.entry("default").unwrap();
        assert!(entry.verification_ready());

        // 幂等：同值再封一次不报错。
        apply_seal(&mut manifest, "default", &proposal).unwrap();

        // 冲突：不同值拒绝改写。
        let mut other = proposal.clone();
        other.bytes += 1;
        assert!(matches!(
            apply_seal(&mut manifest, "default", &other),
            Err(SealError::AlreadySealed { .. })
        ));

        // 未知槽位。
        assert!(matches!(
            apply_seal(&mut manifest, "ghost", &proposal),
            Err(SealError::NotFound(_))
        ));
    }

    #[test]
    fn unsealed_entries_still_fail_verification_ready() {
        // 固化只动被点名的槽位，其余槽位保持 UNVERIFIED 语义。
        let mut manifest = ModelManifest::embedded().unwrap();
        let proposal = parse_tree_metadata(&sample_tree(), "LFM3-1.7B-Q4_K_M.gguf").unwrap();
        apply_seal(&mut manifest, "default", &proposal).unwrap();
        for entry in &manifest.entries {
            if entry.slot == "default" {
                assert!(entry.verification_ready());
            } else {
                assert!(!entry.verification_ready());
            }
        }
    }
}

//! 权重文件的离线校验与导入。
//!
//! 诚实边界：清单未固化 sha256 / 精确字节数时，结果一律是 `Unverifiable`，
//! **绝不输出「校验通过」**。M0 不实现联网下载，避免静默出网。

use crate::manifest::ModelEntry;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyStatus {
    Verified,
    Unverifiable(String),
    Mismatch(String),
}

impl VerifyStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Verified => "VERIFIED",
            Self::Unverifiable(_) => "UNVERIFIED",
            Self::Mismatch(_) => "MISMATCH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub status: VerifyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Io(String),
    Rejected(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "读取权重失败：{msg}"),
            Self::Rejected(msg) => write!(f, "拒绝导入：{msg}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// 流式计算文件大小与 sha256，避免把 1.1 GB 读进内存。
pub fn digest_file(path: &Path) -> Result<(u64, String), VerifyError> {
    let file = std::fs::File::open(path)
        .map_err(|e| VerifyError::Io(format!("{}: {e}", path.display())))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| VerifyError::Io(e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    let digest = hasher.finalize();
    let hex = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((total, hex))
}

pub fn verify_file(path: &Path, entry: &ModelEntry) -> Result<Verification, VerifyError> {
    let (bytes, sha256) = digest_file(path)?;
    let expected_hash = entry.sha256.as_deref();
    let expected_bytes = entry.bytes;

    let status = match (expected_hash, expected_bytes) {
        (Some(expected_hash), Some(expected_bytes)) => {
            if sha256 != expected_hash {
                VerifyStatus::Mismatch(format!("sha256 不符：期望 {expected_hash}，实测 {sha256}"))
            } else if bytes != expected_bytes {
                VerifyStatus::Mismatch(format!("字节数不符：期望 {expected_bytes}，实测 {bytes}"))
            } else {
                VerifyStatus::Verified
            }
        }
        (Some(_), None) => {
            VerifyStatus::Unverifiable("清单未登记精确字节数，仅摘要可比；请固化后重试".into())
        }
        (None, _) => VerifyStatus::Unverifiable(format!(
            "清单未登记 sha256（官方仅公布标称体积 {}）；本轮只报告实测值，不判定通过",
            entry.approx_size
        )),
    };

    Ok(Verification {
        path: path.to_path_buf(),
        bytes,
        sha256,
        status,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub destination: PathBuf,
    pub verification: Verification,
}

/// 离线导入：只做本地复制，不发起任何网络请求。
pub fn import_file(
    source: &Path,
    entry: &ModelEntry,
    cache_root: &Path,
) -> Result<ImportReport, VerifyError> {
    if !entry.runnable {
        return Err(VerifyError::Rejected(format!(
            "槽位 {} 是不可运行的微调底座",
            entry.slot
        )));
    }
    let verification = verify_file(source, entry)?;
    if matches!(verification.status, VerifyStatus::Mismatch(_)) {
        return Err(VerifyError::Rejected(match &verification.status {
            VerifyStatus::Mismatch(msg) => msg.clone(),
            _ => unreachable!(),
        }));
    }

    let destination_dir = cache_root.join(&entry.slot);
    std::fs::create_dir_all(&destination_dir).map_err(|e| VerifyError::Io(e.to_string()))?;
    let destination = destination_dir.join(&entry.file);
    std::fs::copy(source, &destination).map_err(|e| VerifyError::Io(e.to_string()))?;

    Ok(ImportReport {
        destination,
        verification,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelManifest;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("fulu-model-test-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn entry() -> ModelEntry {
        let mut entry = ModelManifest::embedded()
            .unwrap()
            .default_entry()
            .unwrap()
            .clone();
        entry.file = "test.gguf".into();
        entry
    }

    #[test]
    fn missing_digest_is_explicitly_unverified() {
        let path = temp_file("plain.bin", b"hello fulu");
        let result = verify_file(&path, &entry()).unwrap();
        assert_eq!(result.bytes, 10);
        assert_eq!(result.sha256.len(), 64);
        assert!(matches!(result.status, VerifyStatus::Unverifiable(_)));
        assert_eq!(result.status.label(), "UNVERIFIED");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn registered_digest_produces_verified() {
        let path = temp_file("known.bin", b"known content");
        let (bytes, hash) = digest_file(&path).unwrap();
        let mut model = entry();
        model.bytes = Some(bytes);
        model.sha256 = Some(hash);
        let result = verify_file(&path, &model).unwrap();
        assert_eq!(result.status, VerifyStatus::Verified);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn wrong_digest_is_rejected() {
        let path = temp_file("wrong.bin", b"known content");
        let mut model = entry();
        model.bytes = Some(13);
        model.sha256 = Some("0".repeat(64));
        let result = verify_file(&path, &model).unwrap();
        assert!(matches!(result.status, VerifyStatus::Mismatch(_)));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn import_copies_locally_without_network() {
        let path = temp_file("import.bin", b"offline weights");
        let model = entry();
        let mut cache = std::env::temp_dir();
        cache.push(format!("fulu-import-test-{}", std::process::id()));
        let report = import_file(&path, &model, &cache).unwrap();
        assert!(report.destination.exists());
        assert_eq!(
            std::fs::read(&report.destination).unwrap(),
            b"offline weights"
        );
        assert_eq!(report.verification.status.label(), "UNVERIFIED");
        std::fs::remove_file(path).ok();
        std::fs::remove_dir_all(cache).ok();
    }

    #[test]
    fn finetune_base_cannot_be_imported_as_runtime_model() {
        let path = temp_file("base.bin", b"bf16");
        let base = ModelManifest::embedded()
            .unwrap()
            .entry("finetune_base")
            .unwrap()
            .clone();
        let cache = std::env::temp_dir();
        assert!(matches!(
            import_file(&path, &base, &cache),
            Err(VerifyError::Rejected(_))
        ));
        std::fs::remove_file(path).ok();
    }
}

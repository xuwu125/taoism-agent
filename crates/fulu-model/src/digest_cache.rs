//! 模型状态用的 sha256 进程外缓存（按 path + size + mtime 命中）。
//!
//! 背景（docs/02 §39.5.4）：`model list` / `doctor` / GUI 启动每次都对 1.1 GB
//! 权重做全量 sha256，release 实测 ≈ 8.5 s。缓存按「路径 + 字节数 + 修改时间」
//! 三元组命中，任一不符就重算——**重算永远比陈旧值安全**，所以缓存文件损坏、
//! 缺字段、mtime 早于 Unix 纪元等情况一律按未命中处理（这是安全方向的静默，
//! 不会把错摘要当通过；显式校验命令 `model verify` 不走缓存，始终全量哈希）。
//!
//! 已知边界（如实记录）：文件在 mtime 粒度内被改写且大小不变时可能命中陈旧值；
//! 需要**强制全量**时设 `FULU_DIGEST_CACHE=0`。

use crate::manifest::ModelEntry;
use crate::verify::{Verification, verify_file};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 缓存文件位置：模型缓存根目录下的 `digest-cache.json`。
pub fn cache_file() -> PathBuf {
    crate::cache_dir().join("digest-cache.json")
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn mtime_stamp(time: std::time::SystemTime) -> Option<(u64, u32)> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedDigest {
    size: u64,
    mtime_secs: u64,
    mtime_nanos: u32,
    sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CacheFile {
    entries: BTreeMap<String, CachedDigest>,
}

fn cache_disabled() -> bool {
    std::env::var_os("FULU_DIGEST_CACHE")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

fn load(path: &Path) -> CacheFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 原子写回：先写 `.tmp` 再改名，避免半份缓存被下次读走。
fn store(path: &Path, cache: &CacheFile) {
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(text) = serde_json::to_string(cache) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, path);
}

/// 与 `verify_file` 同判据的**带缓存**版本：`status()`（list / doctor / GUI）
/// 用它；显式校验命令必须继续用 `verify_file`（全量哈希，不走任何缓存）。
pub fn verify_file_cached(
    path: &Path,
    entry: &ModelEntry,
    cache_path: &Path,
) -> Result<Verification, crate::verify::VerifyError> {
    let enabled = !cache_disabled();
    verify_file_cached_inner(path, entry, cache_path, enabled)
}

fn verify_file_cached_inner(
    path: &Path,
    entry: &ModelEntry,
    cache_path: &Path,
    cache_enabled: bool,
) -> Result<Verification, crate::verify::VerifyError> {
    if !cache_enabled {
        return verify_file(path, entry);
    }

    // size / mtime 任一取不到（文件消失、mtime 早于纪元）→ 直接全量。
    let Ok(metadata) = std::fs::metadata(path) else {
        return verify_file(path, entry);
    };
    let Some(mtime) = metadata.modified().ok().and_then(mtime_stamp) else {
        return verify_file(path, entry);
    };
    let size = metadata.len();

    let mut cache = load(cache_path);
    let key = normalize(path);
    let hit = cache
        .entries
        .get(&key)
        .filter(|cached| {
            cached.size == size && cached.mtime_secs == mtime.0 && cached.mtime_nanos == mtime.1
        })
        .cloned();

    let (bytes, sha256) = match hit {
        Some(cached) => (cached.size, cached.sha256),
        None => {
            let (bytes, sha256) = crate::verify::digest_file(path)?;
            cache.entries.insert(
                key,
                CachedDigest {
                    size: bytes,
                    mtime_secs: mtime.0,
                    mtime_nanos: mtime.1,
                    sha256: sha256.clone(),
                },
            );
            store(cache_path, &cache);
            (bytes, sha256)
        }
    };

    // 判据与 verify_file 完全一致（单一事实源 judge）：这里只是把「实测摘要」的
    // 获取换成命中或重算。
    let status = crate::verify::judge(bytes, &sha256, entry);

    Ok(Verification {
        path: path.to_path_buf(),
        bytes,
        sha256,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelManifest;
    use crate::verify::VerifyStatus;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("fulu-digest-cache-{}-{name}", std::process::id()));
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
        entry.bytes = None;
        entry.sha256 = None;
        entry
    }

    #[test]
    fn cache_miss_then_hit_returns_same_digest() {
        let path = temp_file("hit.bin", b"cached content");
        let cache_path = temp_file("cache.json", b"");
        let model = entry();

        let first = verify_file_cached(&path, &model, &cache_path).unwrap();
        assert_eq!(first.bytes, 14);
        assert_eq!(first.sha256.len(), 64);
        assert!(matches!(first.status, VerifyStatus::Unverifiable(_)));

        // 缓存文件已写回；第二次应命中（结果一致）
        let second = verify_file_cached(&path, &model, &cache_path).unwrap();
        assert_eq!(first, second);

        // 命中路径真的用了：把缓存里的摘要改掉，返回值跟着变（mtime/size 未变）
        let mut cache: CacheFile =
            serde_json::from_str(&std::fs::read_to_string(&cache_path).unwrap()).unwrap();
        let key = normalize(&path);
        let cached = cache.entries.get_mut(&key).unwrap();
        cached.sha256 = "f".repeat(64);
        std::fs::write(&cache_path, serde_json::to_string(&cache).unwrap()).unwrap();
        let third = verify_file_cached(&path, &model, &cache_path).unwrap();
        assert_eq!(third.sha256, "f".repeat(64));

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[test]
    fn changed_content_recomputes_even_at_same_size() {
        let path = temp_file("mutate.bin", b"original content");
        let cache_path = temp_file("cache2.json", b"");
        let model = entry();
        let first = verify_file_cached(&path, &model, &cache_path).unwrap();

        // 同字节数改写内容：mtime 必变，应重算而不是命中陈旧值
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, b"original contenT").unwrap();
        let second = verify_file_cached(&path, &model, &cache_path).unwrap();
        assert_ne!(first.sha256, second.sha256, "内容变了摘要必须变");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[test]
    fn corrupt_cache_file_is_treated_as_miss_not_truth() {
        let path = temp_file("corrupt.bin", b"honest content");
        let cache_path = temp_file("corrupt-cache.json", b"{ not json");
        let model = entry();
        let result = verify_file_cached(&path, &model, &cache_path).unwrap();
        assert_eq!(result.bytes, 14);
        assert_eq!(result.sha256.len(), 64); // 重算出的真实摘要
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[test]
    fn disabled_cache_bypasses_reading_and_writing() {
        let path = temp_file("switch.bin", b"bypass content");
        let cache_path = temp_file("cache3.json", b"");
        let model = entry();
        let result = verify_file_cached_inner(&path, &model, &cache_path, false).unwrap();
        assert_eq!(result.bytes, 14);
        // 关闭状态下不读也不写缓存文件
        assert_eq!(std::fs::read_to_string(&cache_path).unwrap(), "");
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[test]
    fn missing_file_and_pre_epoch_mtime_fall_back_to_full_hash() {
        // 文件不存在时走 verify_file 的 Io 错误路径，不 panic
        let absent = std::env::temp_dir().join(format!(
            "fulu-digest-cache-absent-{}.bin",
            std::process::id()
        ));
        let cache_path = temp_file("cache4.json", b"");
        assert!(matches!(
            verify_file_cached(&absent, &entry(), &cache_path),
            Err(crate::verify::VerifyError::Io(_))
        ));
        std::fs::remove_file(&cache_path).ok();
    }
}

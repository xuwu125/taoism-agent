//! 箓牒（Vault）：凭据库。**符内只有 `credential_ref`，真密钥只存这里。**
//!
//! 这一层要回答的问题只有一个：给定一个引用（例如 `lu://default/cloud`），
//! 能不能把密钥取出来；取不到就**拒绝执行**，绝不退化成「用空密钥试一次」。
//!
//! 两种后端（由 [`select_backend`] 决定，**不存在静默回退**）：
//!
//! | 后端 | 密钥存在哪 | 谁加密 | 口令 |
//! | --- | --- | --- | --- |
//! | [`SystemVault`] | 系统凭据库（Windows Credential Manager / macOS Keychain / Secret Service） | 操作系统 | 无（跟随登录态） |
//! | [`EncryptedFileVault`] | `~/.fulu/vault/<name>.cred` | 本 crate（ChaCha20-Poly1305 + PBKDF2） | 需要 |
//!
//! 为什么系统凭据库走「委托外部命令」而不是链接原生 API：与 moli 复用系统浏览器、
//! T19 复用系统 `curl` 做 TLS 校验同一路线——不把平台 SDK 拖进依赖树，
//! 也不让 `unsafe` 蔓延到本 crate。代价是要求目标机器上存在对应工具，
//! 缺失时**如实报错**并指路，而不是偷偷写到明文文件里。
//!
//! 红线（与 `fulu-register` 一致）：
//! - 明文密钥**零落盘**：文件后端只写密文，审计与日志只有引用名；
//! - [`Secret`] 的 `Debug` 一律打码、`Drop` 时清零，避免 `{:?}` 顺手把密钥写进日志；
//! - 密钥**不进 argv**（Windows / Linux 经 stdin 传入），macOS 的 `security` 是例外，
//!   已在 docs/03 第 6 节登记为残余风险。

mod file;
mod system;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

pub use file::{EncryptedFileVault, file_format_header};
pub use system::{SystemVault, system_backend_label};

/// 本地模型不需要任何密钥；这个引用永远受箓、且**永不查凭据库**。
pub const LOCAL_REF: &str = "lu://default/local";

/// 文件后端的格式版本。改格式必须同时改这里与 [`file`] 的解析分支。
pub const VAULT_FORMAT_VERSION: &str = "fulu-vault-v1";

/// PBKDF2-HMAC-SHA256 迭代次数。偏高是故意的：解密一次要几十毫秒，
/// 暴力猜测口令的成本却抬到不可接受。
pub const KDF_ITERATIONS: u32 = 600_000;

/// 引用前缀。与 `fulu-core` 的符文校验口径一致。
pub const REF_SCHEME: &str = "lu://";

/// 凭据库错误。**每一种都带可执行指引**，不出现「发生了错误」这种废话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// 引用格式不合法（`lu://<scope>/<name>`）
    BadRef(String),
    /// 引用合法但库里没有这条凭据
    NotFound(String),
    /// 后端不可用（工具缺失 / 无图形会话 / 无口令）
    BackendUnavailable(String),
    /// 口令不对或密文被改过（AEAD 不做区分，避免给出爆破提示）
    CannotDecrypt(String),
    /// 密码学参数不合法（长度、算法）
    Crypto(String),
    /// 文件读写失败
    Io(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRef(detail) => write!(f, "凭据引用不合法：{detail}"),
            Self::NotFound(reference) => write!(
                f,
                "箓牒中没有 `{reference}`；先执行 `fulu vault put {reference}` 存入密钥"
            ),
            Self::BackendUnavailable(detail) => write!(f, "凭据后端不可用：{detail}"),
            Self::CannotDecrypt(detail) => write!(f, "无法解密：{detail}"),
            Self::Crypto(detail) => write!(f, "密码学参数：{detail}"),
            Self::Io(detail) => write!(f, "凭据库 I/O：{detail}"),
        }
    }
}

impl std::error::Error for VaultError {}

/// 凭据引用：`lu://<scope>/<name>`。
///
/// 只做格式与字符集校验；**不判断内容像不像密钥**——那是 `fulu-core` 的事
/// （符里出现密钥字面量应当在成符阶段就被判 `SecretLiteralInCredentialRef`）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CredentialRef {
    raw: String,
    scope: String,
    name: String,
}

impl CredentialRef {
    pub fn parse(raw: &str) -> Result<Self, VaultError> {
        let trimmed = raw.trim();
        if trimmed != raw {
            return Err(VaultError::BadRef(format!(
                "`{raw}` 含首尾空白（多半是从别处复制带进来的）"
            )));
        }
        let rest = trimmed
            .strip_prefix(REF_SCHEME)
            .ok_or_else(|| VaultError::BadRef(format!("`{raw}` 必须以 {REF_SCHEME} 开头")))?;
        let mut parts = rest.split('/');
        let (Some(scope), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(VaultError::BadRef(format!(
                "`{raw}` 必须形如 {REF_SCHEME}<scope>/<name>（恰好两段）"
            )));
        };
        if scope.is_empty() || name.is_empty() {
            return Err(VaultError::BadRef(format!("`{raw}` 的 scope 或 name 为空")));
        }
        check_segment(raw, "scope", scope, 32)?;
        check_segment(raw, "name", name, 64)?;
        Ok(Self {
            raw: trimmed.to_owned(),
            scope: scope.to_owned(),
            name: name.to_owned(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 本地模型引用：不需要也不需要查凭据。
    pub fn is_local(&self) -> bool {
        self.raw == LOCAL_REF
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn check_segment(raw: &str, field: &str, value: &str, max: usize) -> Result<(), VaultError> {
    if value.len() > max {
        return Err(VaultError::BadRef(format!(
            "`{raw}` 的 {field} 超过 {max} 字符"
        )));
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        return Err(VaultError::BadRef(format!(
            "`{raw}` 的 {field} 只允许小写字母、数字、`-`、`_`"
        )));
    }
    Ok(())
}

/// 一段密钥材料。**`Debug` 打码、`Drop` 清零**，不接受任何形式的「顺手打印」。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 唯一取出明文的口子；调用点必须是「马上要用」，不要落到长生命周期变量里。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 连长度都不给：长度本身也是信息。
        f.write_str("Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

/// 凭据库。三种实现：系统后端、文件后端、内存后端（仅测试）。
pub trait Vault: Send + Sync {
    /// 后端名，给 `fulu vault backend` 与 dry-run 用。
    fn backend(&self) -> &'static str;

    fn get(&self, reference: &CredentialRef) -> Result<Secret, VaultError>;

    fn put(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), VaultError>;

    fn delete(&self, reference: &CredentialRef) -> Result<(), VaultError>;

    /// 已登记的引用（**只有名字，没有密钥**）。
    ///
    /// 系统后端无法枚举自己的库（三个平台都没有可移植的枚举接口），因此这里读的是
    /// 一份只含引用名的索引文件；索引存在 ≠ 密钥可取，`get` 仍可能 `NotFound`。
    fn list(&self) -> Result<Vec<String>, VaultError>;
}

/// 内存后端。**仅供测试与演示**，不参与任何真实路径：进程退出即丢，不会落盘。
#[derive(Debug, Default)]
pub struct MemoryVault {
    items: std::sync::Mutex<std::collections::BTreeMap<String, Secret>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_entry(reference: &str, secret: &str) -> Self {
        let vault = Self::new();
        let parsed = CredentialRef::parse(reference).expect("测试用引用必须合法");
        vault
            .items
            .lock()
            .expect("内存凭据库锁不可中毒")
            .insert(parsed.as_str().to_owned(), Secret::new(secret));
        vault
    }
}

impl Vault for MemoryVault {
    fn backend(&self) -> &'static str {
        "memory（内存，仅测试）"
    }

    fn get(&self, reference: &CredentialRef) -> Result<Secret, VaultError> {
        self.items
            .lock()
            .expect("内存凭据库锁不可中毒")
            .get(reference.as_str())
            .cloned()
            .ok_or_else(|| VaultError::NotFound(reference.as_str().to_owned()))
    }

    fn put(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), VaultError> {
        self.items
            .lock()
            .expect("内存凭据库锁不可中毒")
            .insert(reference.as_str().to_owned(), secret.clone());
        Ok(())
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), VaultError> {
        let removed = self
            .items
            .lock()
            .expect("内存凭据库锁不可中毒")
            .remove(reference.as_str());
        match removed {
            Some(_) => Ok(()),
            None => Err(VaultError::NotFound(reference.as_str().to_owned())),
        }
    }

    fn list(&self) -> Result<Vec<String>, VaultError> {
        Ok(self
            .items
            .lock()
            .expect("内存凭据库锁不可中毒")
            .keys()
            .cloned()
            .collect())
    }
}

/// `fulu` 的主目录：`FULU_HOME` 优先，其次 `~/.fulu`。
///
/// 拆成纯函数是为了能测：直接读环境变量的函数在并行测试里没法安全断言。
pub fn fulu_home_from(
    explicit: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        let path = PathBuf::from(explicit);
        return (!path.as_os_str().is_empty()).then_some(path);
    }
    home.filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".fulu"))
}

/// 当前用户的 `fulu` 主目录（`FULU_HOME` / `HOME` / `USERPROFILE`）。
pub fn fulu_home() -> Option<PathBuf> {
    let explicit = std::env::var_os("FULU_HOME");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    fulu_home_from(explicit.as_deref(), home.as_deref())
}

/// 索引文件：只记引用名，绝不记密钥。缺它不影响取密钥，只影响 `list`。
pub fn index_path_from(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join("vault-index.json"))
}

pub fn index_path() -> Option<PathBuf> {
    index_path_from(fulu_home().as_deref())
}

/// 读索引。文件缺失 = 空索引（不是错误）；内容坏掉 = 空索引 + 不覆盖原文件。
pub fn read_index(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_str::<Vec<String>>(&text) else {
        return Vec::new();
    };
    // 顺手剔掉此刻已不合法的旧条目，避免坏引用在后面反复冒出来。
    list.into_iter()
        .filter(|item| CredentialRef::parse(item).is_ok())
        .collect()
}

pub(crate) fn write_index(path: &Path, refs: &BTreeSet<String>) -> Result<(), VaultError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VaultError::Io(format!("创建 {} 失败：{e}", parent.display())))?;
    }
    let list: Vec<&String> = refs.iter().collect();
    let body = serde_json::to_string_pretty(&list)
        .map_err(|e| VaultError::Io(format!("索引序列化失败：{e}")))?;
    std::fs::write(path, format!("{body}\n"))
        .map_err(|e| VaultError::Io(format!("写 {} 失败：{e}", path.display())))
}

pub(crate) fn index_add(path: &Path, reference: &str) -> Result<(), VaultError> {
    let mut refs: BTreeSet<String> = read_index(path).into_iter().collect();
    if refs.insert(reference.to_owned()) {
        write_index(path, &refs)?;
    }
    Ok(())
}

pub(crate) fn index_remove(path: &Path, reference: &str) -> Result<(), VaultError> {
    let mut refs: BTreeSet<String> = read_index(path).into_iter().collect();
    if refs.remove(reference) {
        write_index(path, &refs)?;
    }
    Ok(())
}

/// 后端选择：**有口令就用文件后端，没有就用系统凭据库**。
///
/// 刻意不做「系统后端失败就退回文件后端」：静默换后端等于静默换信任模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendChoice {
    System,
    File,
}

pub fn select_backend(passphrase: Option<&str>) -> BackendChoice {
    match passphrase {
        Some(text) if !text.is_empty() => BackendChoice::File,
        _ => BackendChoice::System,
    }
}

/// 按当前环境打开默认箓牒。
///
/// `FULU_VAULT_PASSPHRASE` 非空 → 文件后端（`~/.fulu/vault`）；否则系统凭据库。
pub fn open_default() -> Result<Box<dyn Vault>, VaultError> {
    let passphrase = std::env::var("FULU_VAULT_PASSPHRASE").ok();
    let home = fulu_home();
    open_with(passphrase.as_deref(), home.as_deref())
}

/// [`open_default`] 的可测版本：口令与主目录都由调用方给出。
pub fn open_with(
    passphrase: Option<&str>,
    home: Option<&Path>,
) -> Result<Box<dyn Vault>, VaultError> {
    match select_backend(passphrase) {
        BackendChoice::File => {
            let home = home.ok_or_else(|| {
                VaultError::BackendUnavailable(
                    "设了 FULU_VAULT_PASSPHRASE 但找不到用户主目录（HOME / USERPROFILE 均为空）"
                        .into(),
                )
            })?;
            let vault = EncryptedFileVault::new(
                home.join("vault"),
                Secret::new(passphrase.unwrap_or_default()),
            );
            Ok(Box::new(vault))
        }
        BackendChoice::System => {
            let index = index_path_from(home);
            Ok(Box::new(SystemVault::new(index)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_must_be_two_clean_segments() {
        for good in [
            LOCAL_REF,
            "lu://default/cloud",
            "lu://work/openai-key",
            "lu://a/b",
        ] {
            assert!(CredentialRef::parse(good).is_ok(), "{good} 应当合法");
        }
        for bad in [
            "",
            "lu://",
            "lu://only-scope",
            "lu://a/b/c",
            "lu:///name",
            "lu://scope/",
            "http://default/cloud",
            "lu://Default/Cloud",
            "lu://default/cl oud",
            " lu://default/cloud",
            "lu://default/cloud ",
            "lu://default/cl@ud",
        ] {
            assert!(CredentialRef::parse(bad).is_err(), "{bad:?} 应当被拒");
        }
    }

    #[test]
    fn segment_lengths_are_bounded() {
        let long_scope = format!("lu://{}/k", "a".repeat(33));
        let long_name = format!("lu://s/{}", "a".repeat(65));
        assert!(CredentialRef::parse(&long_scope).is_err());
        assert!(CredentialRef::parse(&long_name).is_err());
    }

    #[test]
    fn local_reference_is_recognised() {
        assert!(CredentialRef::parse(LOCAL_REF).expect("合法").is_local());
        assert!(
            !CredentialRef::parse("lu://default/cloud")
                .expect("合法")
                .is_local()
        );
    }

    #[test]
    fn secret_debug_never_leaks_the_value() {
        let secret = Secret::new("sk-proj-abcdefghijklmnop");
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret(***)");
        assert!(!rendered.contains("sk-"));
        // 连带长度的暗示都不要
        assert!(!rendered.contains("24"));
    }

    #[test]
    fn memory_vault_round_trips_and_lists_only_references() {
        let vault = MemoryVault::with_entry("lu://default/cloud", "sk-1");
        let reference = CredentialRef::parse("lu://default/cloud").expect("合法");
        assert_eq!(vault.get(&reference).expect("应取到").expose(), "sk-1");
        assert_eq!(
            vault.list().expect("可枚举"),
            vec!["lu://default/cloud".to_owned()]
        );

        let other = CredentialRef::parse("lu://default/absent").expect("合法");
        assert!(matches!(vault.get(&other), Err(VaultError::NotFound(_))));
        assert!(matches!(vault.delete(&other), Err(VaultError::NotFound(_))));
    }

    #[test]
    fn backend_choice_is_explicit_and_never_falls_back() {
        assert_eq!(select_backend(None), BackendChoice::System);
        assert_eq!(select_backend(Some("")), BackendChoice::System);
        assert_eq!(select_backend(Some("hunter2")), BackendChoice::File);
    }

    #[test]
    fn home_resolution_prefers_explicit_then_home() {
        use std::ffi::OsStr;
        assert_eq!(
            fulu_home_from(Some(OsStr::new("/tmp/f")), Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/tmp/f"))
        );
        assert_eq!(
            fulu_home_from(None, Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/home/u").join(".fulu"))
        );
        assert_eq!(fulu_home_from(None, None), None);
        assert_eq!(
            fulu_home_from(Some(OsStr::new("")), None),
            None,
            "空的 FULU_HOME 应视作未设置，而不是当前目录"
        );
    }

    #[test]
    fn index_round_trips_and_drops_malformed_entries() {
        let dir = std::env::temp_dir().join(format!("fulu-vault-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("vault-index.json");

        assert!(read_index(&path).is_empty(), "缺文件应视为空索引");

        index_add(&path, "lu://default/cloud").expect("写入");
        index_add(&path, "lu://default/cloud").expect("重复写入应幂等");
        index_add(&path, "lu://work/proxy").expect("写入");
        assert_eq!(
            read_index(&path),
            vec![
                "lu://default/cloud".to_owned(),
                "lu://work/proxy".to_owned()
            ]
        );

        // 手写一条非法引用：读的时候应被剔掉，而不是让后面的流程去踩。
        std::fs::write(&path, r#"["lu://default/cloud","not-a-ref"]"#).expect("写坏数据");
        assert_eq!(read_index(&path), vec!["lu://default/cloud".to_owned()]);

        index_remove(&path, "lu://default/cloud").expect("删除");
        assert!(read_index(&path).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_without_passphrase_still_opens_the_system_backend() {
        // 系统后端在构造时不得起进程、不得访问凭据库：只有 get/put 才碰系统。
        let vault = open_with(None, Some(Path::new("/tmp/fulu-test-home"))).expect("应可构造");
        assert!(vault.backend().contains("系统"));
    }

    #[test]
    fn file_backend_requires_a_home_and_a_passphrase() {
        assert!(matches!(
            open_with(Some("pw"), None),
            Err(VaultError::BackendUnavailable(_))
        ));
    }
}

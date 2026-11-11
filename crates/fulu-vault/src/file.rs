//! 文件级加密后端：系统凭据库不可用时的兜底（headless Linux、CI、受限 Windows）。
//!
//! 格式（UTF-8 文本，一行一个字段，便于人工确认「这里面确实没有明文」）：
//!
//! ```text
//! fulu-vault-v1
//! ref lu://default/cloud
//! kdf pbkdf2-sha256:600000
//! salt <32 位十六进制>
//! nonce <24 位十六进制>
//! ct <十六进制密文 + 16 字节 tag>
//! ```
//!
//! 密码学口径（不自己发明）：
//! - KDF：PBKDF2-HMAC-SHA256，600000 次迭代，16 字节随机盐；
//! - AEAD：ChaCha20-Poly1305，12 字节随机 nonce；
//! - **AAD 绑定引用名**：把 `a.cred` 改名成 `b.cred` 也解不开，防止「密钥串用」。
//!
//! 一处刻意的限制：`kdf` 行里的迭代次数在读的时候会被夹到 `[1000, 10_000_000]`，
//! 免得一个手改的畸形文件让 fulu 拿 10 亿次迭代把自己算死。

use crate::{CredentialRef, KDF_ITERATIONS, Secret, VAULT_FORMAT_VERSION, Vault, VaultError};
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

/// 迭代次数的合法区间，防止畸形文件把进程算死。
const MIN_ITERATIONS: u32 = 1_000;
const MAX_ITERATIONS: u32 = 10_000_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// 把文件里声明的迭代次数夹进合法区间。文件是用户可改的，不能无条件相信。
fn clamp_iterations(raw: u32) -> u32 {
    raw.clamp(MIN_ITERATIONS, MAX_ITERATIONS)
}

/// 供文档与测试引用的格式头。
pub fn file_format_header() -> &'static str {
    VAULT_FORMAT_VERSION
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("半字节 < 16"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("半字节 < 16"));
    }
    out
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(u8::try_from(hi * 16 + lo).ok()?);
    }
    Some(out)
}

/// AAD：格式版本 + 引用名。改名即解不开。
fn aad(reference: &CredentialRef) -> String {
    format!("{VAULT_FORMAT_VERSION}|{}", reference.as_str())
}

fn derive_key(passphrase: &Secret, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0_u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.expose().as_bytes(), salt, iterations, &mut key);
    key
}

/// 加密一段明文，产出完整文件正文。返回的字符串里**只有密文**。
pub fn encrypt_to_text(
    reference: &CredentialRef,
    passphrase: &Secret,
    plaintext: &Secret,
    iterations: u32,
) -> Result<String, VaultError> {
    if passphrase.is_empty() {
        return Err(VaultError::BackendUnavailable(
            "文件后端需要口令（设 FULU_VAULT_PASSPHRASE 或 `fulu vault put --ask-passphrase`）；\
             空口令等于不加密，因此拒绝"
                .into(),
        ));
    }
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce = [0_u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);

    let mut key = derive_key(passphrase, &salt, iterations);
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| VaultError::Crypto(format!("密钥长度不合法：{e}")))?;
    key.zeroize();

    let ciphertext = cipher
        .encrypt(
            &nonce.into(),
            Payload {
                msg: plaintext.expose().as_bytes(),
                aad: aad(reference).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto("加密失败".into()))?;

    Ok(format!(
        "{VAULT_FORMAT_VERSION}\nref {}\nkdf pbkdf2-sha256:{iterations}\nsalt {}\nnonce {}\nct {}\n",
        reference.as_str(),
        to_hex(&salt),
        to_hex(&nonce),
        to_hex(&ciphertext)
    ))
}

/// 解密文件正文。任何一步不符都只给一个笼统的「解不开」，不区分「口令错」与「密文被改」。
pub fn decrypt_from_text(
    reference: &CredentialRef,
    passphrase: &Secret,
    body: &str,
) -> Result<Secret, VaultError> {
    let mut lines = body.lines();
    let header = lines.next().unwrap_or_default().trim();
    if header != VAULT_FORMAT_VERSION {
        return Err(VaultError::CannotDecrypt(format!(
            "格式头不是 {VAULT_FORMAT_VERSION}（这个文件不是 fulu 箓牒，或版本不兼容）"
        )));
    }
    let mut stored_ref = None;
    let mut iterations = None;
    let mut salt = None;
    let mut nonce = None;
    let mut ciphertext = None;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(' ')
            .ok_or_else(|| VaultError::CannotDecrypt(format!("字段行缺值：`{line}`")))?;
        match key {
            "ref" => stored_ref = Some(value.to_owned()),
            "kdf" => {
                let spec = value
                    .strip_prefix("pbkdf2-sha256:")
                    .ok_or_else(|| VaultError::CannotDecrypt(format!("不支持的 KDF：`{value}`")))?;
                let rounds: u32 = spec
                    .parse()
                    .map_err(|_| VaultError::CannotDecrypt(format!("迭代次数不合法：`{spec}`")))?;
                iterations = Some(clamp_iterations(rounds));
            }
            "salt" => {
                salt = Some(
                    from_hex(value)
                        .ok_or_else(|| VaultError::CannotDecrypt("salt 不是十六进制".into()))?,
                );
            }
            "nonce" => {
                nonce = Some(
                    from_hex(value)
                        .ok_or_else(|| VaultError::CannotDecrypt("nonce 不是十六进制".into()))?,
                );
            }
            "ct" => {
                ciphertext = Some(
                    from_hex(value)
                        .ok_or_else(|| VaultError::CannotDecrypt("密文不是十六进制".into()))?,
                );
            }
            other => {
                return Err(VaultError::CannotDecrypt(format!("未知字段 `{other}`")));
            }
        }
    }

    let stored_ref = stored_ref.ok_or_else(|| VaultError::CannotDecrypt("缺 ref 字段".into()))?;
    // 文件名与文件内声明必须一致，否则说明有人挪过文件。
    if stored_ref != reference.as_str() {
        return Err(VaultError::CannotDecrypt(format!(
            "文件声明的引用是 `{stored_ref}`，与预期的 `{}` 不一致",
            reference.as_str()
        )));
    }
    let salt = salt.ok_or_else(|| VaultError::CannotDecrypt("缺 salt".into()))?;
    let nonce = nonce.ok_or_else(|| VaultError::CannotDecrypt("缺 nonce".into()))?;
    let ciphertext = ciphertext.ok_or_else(|| VaultError::CannotDecrypt("缺密文".into()))?;
    let iterations = iterations.ok_or_else(|| VaultError::CannotDecrypt("缺 kdf".into()))?;
    if salt.len() != SALT_LEN || nonce.len() != NONCE_LEN {
        return Err(VaultError::CannotDecrypt("salt / nonce 长度不对".into()));
    }

    let mut key = derive_key(passphrase, &salt, iterations);
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| VaultError::CannotDecrypt(format!("密钥长度不合法：{e}")))?;
    key.zeroize();

    let nonce_array: [u8; NONCE_LEN] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::CannotDecrypt("nonce 长度不对".into()))?;
    let plaintext = cipher
        .decrypt(
            &nonce_array.into(),
            Payload {
                msg: &ciphertext,
                aad: aad(reference).as_bytes(),
            },
        )
        .map_err(|_| VaultError::CannotDecrypt("口令不对，或文件被改过（AEAD 校验失败）".into()))?;
    let text = String::from_utf8(plaintext)
        .map_err(|_| VaultError::CannotDecrypt("解出的内容不是 UTF-8".into()))?;
    Ok(Secret::new(text))
}

#[derive(Debug)]
pub struct EncryptedFileVault {
    dir: PathBuf,
    passphrase: Secret,
    /// 写入时用的迭代次数。生产固定 [`KDF_ITERATIONS`]；测试注入小值以免 debug 下算到天荒地老。
    iterations: u32,
}

impl EncryptedFileVault {
    pub fn new(dir: PathBuf, passphrase: Secret) -> Self {
        Self::with_iterations(dir, passphrase, KDF_ITERATIONS)
    }

    pub fn with_iterations(dir: PathBuf, passphrase: Secret, iterations: u32) -> Self {
        Self {
            dir,
            passphrase,
            iterations: clamp_iterations(iterations),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    /// 每个 scope 一个子目录：scope 与 name 都只允许 `[a-z0-9_-]`，
    /// 因此既不会碰撞、也不会出现路径穿越。
    fn path_for(&self, reference: &CredentialRef) -> PathBuf {
        self.dir
            .join(reference.scope())
            .join(format!("{}.cred", reference.name()))
    }

    fn write_private(path: &Path, body: &str) -> Result<(), VaultError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| VaultError::Io(format!("创建 {} 失败：{e}", parent.display())))?;
            restrict_permissions(parent, true)?;
        }
        // 先写临时文件再改名：中途失败不会留下半个密文。
        let temp = path.with_extension("cred.tmp");
        std::fs::write(&temp, body)
            .map_err(|e| VaultError::Io(format!("写 {} 失败：{e}", temp.display())))?;
        restrict_permissions(&temp, false)?;
        std::fs::rename(&temp, path).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            VaultError::Io(format!("落盘 {} 失败：{e}", path.display()))
        })
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, is_dir: bool) -> Result<(), VaultError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if is_dir { 0o700 } else { 0o600 };
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions)
        .map_err(|e| VaultError::Io(format!("设置 {} 权限失败：{e}", path.display())))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _is_dir: bool) -> Result<(), VaultError> {
    // Windows 上没有 chmod 语义；密文本身由 AEAD 保护，权限收紧交由用户目录 ACL。
    Ok(())
}

impl Vault for EncryptedFileVault {
    fn backend(&self) -> &'static str {
        "文件级加密（ChaCha20-Poly1305 + PBKDF2，需口令）"
    }

    fn get(&self, reference: &CredentialRef) -> Result<Secret, VaultError> {
        let path = self.path_for(reference);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(VaultError::NotFound(reference.as_str().to_owned()));
            }
            Err(error) => {
                return Err(VaultError::Io(format!(
                    "读 {} 失败：{error}",
                    path.display()
                )));
            }
        };
        decrypt_from_text(reference, &self.passphrase, &body)
    }

    fn put(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), VaultError> {
        if secret.is_empty() {
            return Err(VaultError::BackendUnavailable(
                "拒绝存入空密钥：空密钥等于关掉认证，不如不存".into(),
            ));
        }
        let body = encrypt_to_text(reference, &self.passphrase, secret, self.iterations)?;
        Self::write_private(&self.path_for(reference), &body)
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), VaultError> {
        let path = self.path_for(reference);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(VaultError::NotFound(reference.as_str().to_owned()))
            }
            Err(error) => Err(VaultError::Io(format!(
                "删 {} 失败：{error}",
                path.display()
            ))),
        }
    }

    fn list(&self) -> Result<Vec<String>, VaultError> {
        // 文件后端可以直接枚举磁盘：比索引更可信（文件在，索引不一定对）。
        let mut found = Vec::new();
        let Ok(scopes) = std::fs::read_dir(&self.dir) else {
            return Ok(found);
        };
        for scope in scopes.flatten() {
            if !scope.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(scope_name) = scope.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(names) = std::fs::read_dir(scope.path()) else {
                continue;
            };
            for name in names.flatten() {
                let Some(file_name) = name.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(stem) = file_name.strip_suffix(".cred") else {
                    continue;
                };
                let candidate = format!("lu://{scope_name}/{stem}");
                if CredentialRef::parse(&candidate).is_ok() {
                    found.push(candidate);
                }
            }
        }
        found.sort();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> CredentialRef {
        CredentialRef::parse("lu://default/cloud").expect("合法引用")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fulu-vault-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn hex_helpers_round_trip_and_reject_garbage() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(from_hex("000fff"), Some(vec![0x00, 0x0f, 0xff]));
        assert_eq!(from_hex("000ff"), None, "奇数长度必须拒绝");
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn ciphertext_contains_no_plaintext() {
        let passphrase = Secret::new("hunter2");
        let secret = Secret::new("sk-proj-abcdefghijklmnop0123");
        let body = encrypt_to_text(&reference(), &passphrase, &secret, 1_000).expect("加密");
        assert!(body.starts_with(VAULT_FORMAT_VERSION));
        assert!(!body.contains("sk-proj"));
        assert!(!body.contains("hunter2"), "口令绝不能进文件");
        assert!(body.contains("ref lu://default/cloud"));
    }

    #[test]
    fn round_trip_succeeds_with_the_right_passphrase() {
        let passphrase = Secret::new("hunter2");
        let secret = Secret::new("sk-proj-abcdefghijklmnop0123");
        let body = encrypt_to_text(&reference(), &passphrase, &secret, 1_000).expect("加密");
        let back = decrypt_from_text(&reference(), &passphrase, &body).expect("解密");
        assert_eq!(back.expose(), secret.expose());
    }

    #[test]
    fn wrong_passphrase_is_refused() {
        let secret = Secret::new("sk-1");
        let body =
            encrypt_to_text(&reference(), &Secret::new("right"), &secret, 1_000).expect("加密");
        match decrypt_from_text(&reference(), &Secret::new("wrong"), &body) {
            Err(VaultError::CannotDecrypt(_)) => {}
            other => panic!("口令错必须拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn swapped_reference_cannot_be_decrypted() {
        // AAD 绑定引用：把同一条密文当另一条凭据用必须失败。
        let passphrase = Secret::new("pw");
        let body =
            encrypt_to_text(&reference(), &passphrase, &Secret::new("sk-1"), 1_000).expect("加密");
        let other = CredentialRef::parse("lu://default/other").expect("合法");
        assert!(matches!(
            decrypt_from_text(&other, &passphrase, &body),
            Err(VaultError::CannotDecrypt(_))
        ));
    }

    #[test]
    fn tampered_ciphertext_is_refused() {
        let passphrase = Secret::new("pw");
        let body =
            encrypt_to_text(&reference(), &passphrase, &Secret::new("sk-1"), 1_000).expect("加密");
        let flipped = body.replace("ct ", "ct 0");
        // 长度变了必然解不开；这里只断言「拒绝」而不是「可能拒绝」。
        assert!(matches!(
            decrypt_from_text(&reference(), &passphrase, &flipped),
            Err(VaultError::CannotDecrypt(_))
        ));
    }

    #[test]
    fn malformed_bodies_are_rejected_field_by_field() {
        let passphrase = Secret::new("pw");
        for body in [
            "",
            "not-a-vault\n",
            "fulu-vault-v1\n",
            "fulu-vault-v1\nref lu://default/cloud\n",
            "fulu-vault-v1\nref lu://default/cloud\nkdf scrypt:1\n",
            "fulu-vault-v1\nref lu://default/cloud\nkdf pbkdf2-sha256:abc\n",
            "fulu-vault-v1\nref lu://default/cloud\nkdf pbkdf2-sha256:600000\nsalt xyz\n",
            "fulu-vault-v1\nref lu://default/cloud\nkdf pbkdf2-sha256:600000\nsalt 00\nnonce 00\nct 00\n",
        ] {
            assert!(
                matches!(
                    decrypt_from_text(&reference(), &passphrase, body),
                    Err(VaultError::CannotDecrypt(_)) | Err(VaultError::Io(_))
                ),
                "畸形正文必须拒绝：{body:?}"
            );
        }
    }

    #[test]
    fn iteration_count_is_clamped_so_a_crafted_file_cannot_pin_the_cpu() {
        // 纯函数直接测边界：4e9 次迭代若不夹住，等于让 fulu 自己把自己算死。
        assert_eq!(clamp_iterations(4_000_000_000), MAX_ITERATIONS);
        assert_eq!(clamp_iterations(0), MIN_ITERATIONS);
        assert_eq!(clamp_iterations(1), MIN_ITERATIONS);
        assert_eq!(clamp_iterations(600_000), 600_000, "区间内的值原样保留");
        assert_eq!(clamp_iterations(MAX_ITERATIONS + 1), MAX_ITERATIONS);
    }

    #[test]
    fn kdf_line_is_honoured_so_retuned_iterations_break_decryption() {
        // 迭代次数被改过就解不开——这证明文件里的 kdf 行确实参与派生，而不是被忽略。
        let passphrase = Secret::new("pw");
        let body =
            encrypt_to_text(&reference(), &passphrase, &Secret::new("x"), 1_000).expect("加密");
        let retuned = body.replace("pbkdf2-sha256:1000", "pbkdf2-sha256:2000");
        assert!(matches!(
            decrypt_from_text(&reference(), &passphrase, &retuned),
            Err(VaultError::CannotDecrypt(_))
        ));
    }

    #[test]
    fn empty_passphrase_is_refused_at_write_time() {
        match encrypt_to_text(&reference(), &Secret::new(""), &Secret::new("x"), 1_000) {
            Err(VaultError::BackendUnavailable(detail)) => {
                assert!(detail.contains("FULU_VAULT_PASSPHRASE"));
            }
            other => panic!("空口令必须拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn file_backend_round_trips_on_disk_and_lists_refs() {
        let dir = temp_dir("disk");
        let vault = EncryptedFileVault::with_iterations(dir.clone(), Secret::new("pw"), 1_000);
        let reference = reference();

        assert!(matches!(
            vault.get(&reference),
            Err(VaultError::NotFound(_))
        ));
        vault.put(&reference, &Secret::new("sk-1")).expect("存");

        // 磁盘上不得出现明文
        let raw = std::fs::read_to_string(dir.join("default").join("cloud.cred")).expect("读文件");
        assert!(!raw.contains("sk-1"));

        assert_eq!(vault.get(&reference).expect("取").expose(), "sk-1");
        assert_eq!(
            vault.list().expect("枚举"),
            vec!["lu://default/cloud".to_owned()]
        );

        // 换一个口令的实例：解不开
        let wrong = EncryptedFileVault::with_iterations(dir.clone(), Secret::new("other"), 1_000);
        assert!(matches!(
            wrong.get(&reference),
            Err(VaultError::CannotDecrypt(_))
        ));

        vault.delete(&reference).expect("删");
        assert!(matches!(
            vault.get(&reference),
            Err(VaultError::NotFound(_))
        ));
        assert!(vault.list().expect("枚举").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scopes_get_their_own_directory_so_names_cannot_collide() {
        let dir = temp_dir("scope");
        let vault = EncryptedFileVault::with_iterations(dir.clone(), Secret::new("pw"), 1_000);
        let a = CredentialRef::parse("lu://a/b__c").expect("合法");
        let b = CredentialRef::parse("lu://a__b/c").expect("合法");
        vault.put(&a, &Secret::new("one")).expect("存 a");
        vault.put(&b, &Secret::new("two")).expect("存 b");
        assert_eq!(vault.get(&a).expect("取 a").expose(), "one");
        assert_eq!(vault.get(&b).expect("取 b").expose(), "two");
        assert_eq!(vault.list().expect("枚举").len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_files_do_not_survive_a_successful_write() {
        let dir = temp_dir("tmpfile");
        let vault = EncryptedFileVault::with_iterations(dir.clone(), Secret::new("pw"), 1_000);
        vault.put(&reference(), &Secret::new("sk-1")).expect("存");
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("default"))
            .expect("读目录")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不该留下临时文件：{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

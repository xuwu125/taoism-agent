//! 系统凭据库后端：**委托 OS 自带工具**，不链接平台 SDK。
//!
//! | 平台 | 工具 | 取 | 存 | 删 |
//! | --- | --- | --- | --- | --- |
//! | Windows | `powershell.exe` + WinRT `PasswordVault` | 脚本读凭据、明文写 stdout | 密钥经 **stdin** | 脚本删条目 |
//! | macOS | `security` | `find-generic-password -w` | `add-generic-password -U -w`（密钥在 argv） | `delete-generic-password` |
//! | Linux | `secret-tool` | `lookup` | `store`（密钥经 **stdin**） | `clear` |
//!
//! 三条路线都不完美，取舍写在 docs/03 第 6 节：
//! - Windows 的脚本是**常量**，用户给的引用名只经环境变量 `FULU_VAULT_USER` 传入，
//!   因此不存在引号/换行注入的面；
//! - macOS 的 `security` 只能从 argv 收密钥（同用户下 `ps` 可见），登记为残余风险；
//! - Linux 的 `secret-tool` 需要 D-Bus 会话，headless 环境下会失败——那种情况该用
//!   文件后端（设 `FULU_VAULT_PASSPHRASE`），而不是让 fulu 去猜。

use crate::{CredentialRef, Secret, Vault, VaultError, index_add, index_remove, read_index};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// 凭据条目在系统库里的固定服务名。
pub const SERVICE: &str = "fulu";

/// 引用名经此环境变量传给外部工具——**密钥绝不走环境变量**（同用户可读 `/proc/self/environ`）。
pub const USER_ENV: &str = "FULU_VAULT_USER";

/// 目标平台。显式建模是为了能在任意平台上测三条分支的构造结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

pub fn current_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    }
}

pub fn system_backend_label(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "系统凭据库（Windows Credential Manager）",
        Platform::MacOs => "系统凭据库（macOS Keychain）",
        Platform::Linux => "系统凭据库（Secret Service / secret-tool）",
    }
}

/// 一次外部调用的完整计划。**可构造即可测**：不执行进程也能断言密钥不在 argv 里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: String,
    pub args: Vec<String>,
    /// 密钥从 stdin 传入（写完后关闭 stdin，不加换行）
    pub secret_on_stdin: bool,
    /// 密钥出现在 argv 里（只有 macOS 会；调用方须在文档中登记）
    pub secret_in_argv: bool,
    pub env: Vec<(String, String)>,
    /// 找不到条目时的期望退出码
    pub not_found_exit: i32,
}

// Windows 脚本是常量：引用名走环境变量，密钥走 stdin，没有任何字符串拼接。
const WINDOWS_GET: &str = r#"
$ErrorActionPreference = 'Stop'
try { [Windows.Security.Credentials.PasswordVault, Windows.Security.Credentials, ContentType = WindowsRuntime] | Out-Null } catch { exit 4 }
$u = $env:FULU_VAULT_USER
try {
  $v = [Windows.Security.Credentials.PasswordVault]::new()
  $hit = $null
  foreach ($c in $v.RetrieveAll()) { if ($c.Resource -eq 'fulu' -and $c.UserName -eq $u) { $hit = $c; break } }
  if ($null -eq $hit) { exit 3 }
  $hit.RetrievePassword()
  [Console]::Out.Write($hit.Password)
} catch { exit 4 }
"#;

const WINDOWS_PUT: &str = r#"
$ErrorActionPreference = 'Stop'
try { [Windows.Security.Credentials.PasswordVault, Windows.Security.Credentials, ContentType = WindowsRuntime] | Out-Null } catch { exit 4 }
$u = $env:FULU_VAULT_USER
$s = [Console]::In.ReadToEnd()
if ([string]::IsNullOrEmpty($s)) { exit 5 }
try {
  $v = [Windows.Security.Credentials.PasswordVault]::new()
  try { $v.Remove($v.Retrieve('fulu', $u)) } catch { }
  $v.Add([Windows.Security.Credentials.PasswordCredential]::new('fulu', $u, $s))
} catch { exit 4 }
exit 0
"#;

const WINDOWS_DELETE: &str = r#"
$ErrorActionPreference = 'Stop'
try { [Windows.Security.Credentials.PasswordVault, Windows.Security.Credentials, ContentType = WindowsRuntime] | Out-Null } catch { exit 4 }
$u = $env:FULU_VAULT_USER
try {
  $v = [Windows.Security.Credentials.PasswordVault]::new()
  $v.Remove($v.Retrieve('fulu', $u))
} catch { exit 3 }
exit 0
"#;

fn powershell(script: &str, name: &str, secret_on_stdin: bool) -> CommandPlan {
    CommandPlan {
        program: "powershell.exe".to_owned(),
        args: vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-Command".to_owned(),
            script.trim().to_owned(),
        ],
        secret_on_stdin,
        secret_in_argv: false,
        env: vec![(USER_ENV.to_owned(), name.to_owned())],
        not_found_exit: 3,
    }
}

/// 取凭据的调用计划。
pub fn plan_get(platform: Platform, name: &str) -> CommandPlan {
    match platform {
        Platform::Windows => powershell(WINDOWS_GET, name, false),
        Platform::MacOs => CommandPlan {
            program: "security".to_owned(),
            args: vec![
                "find-generic-password".to_owned(),
                "-s".to_owned(),
                SERVICE.to_owned(),
                "-a".to_owned(),
                name.to_owned(),
                "-w".to_owned(),
            ],
            secret_on_stdin: false,
            secret_in_argv: false,
            env: Vec::new(),
            // security: errSecItemNotFound = 44
            not_found_exit: 44,
        },
        Platform::Linux => CommandPlan {
            program: "secret-tool".to_owned(),
            args: vec![
                "lookup".to_owned(),
                "service".to_owned(),
                SERVICE.to_owned(),
                "account".to_owned(),
                name.to_owned(),
            ],
            secret_on_stdin: false,
            secret_in_argv: false,
            env: Vec::new(),
            not_found_exit: 1,
        },
    }
}

/// 存凭据的调用计划。**只有 macOS 会把密钥放进 argv**，其余走 stdin。
pub fn plan_put(platform: Platform, name: &str, secret: &Secret) -> CommandPlan {
    match platform {
        Platform::Windows => powershell(WINDOWS_PUT, name, true),
        Platform::MacOs => CommandPlan {
            program: "security".to_owned(),
            args: vec![
                "add-generic-password".to_owned(),
                "-U".to_owned(),
                "-s".to_owned(),
                SERVICE.to_owned(),
                "-a".to_owned(),
                name.to_owned(),
                "-w".to_owned(),
                secret.expose().to_owned(),
            ],
            secret_on_stdin: false,
            secret_in_argv: true,
            env: Vec::new(),
            not_found_exit: 44,
        },
        Platform::Linux => CommandPlan {
            program: "secret-tool".to_owned(),
            args: vec![
                "store".to_owned(),
                "--label".to_owned(),
                SERVICE.to_owned(),
                "service".to_owned(),
                SERVICE.to_owned(),
                "account".to_owned(),
                name.to_owned(),
            ],
            secret_on_stdin: true,
            secret_in_argv: false,
            env: Vec::new(),
            not_found_exit: 1,
        },
    }
}

/// 删凭据的调用计划。
pub fn plan_delete(platform: Platform, name: &str) -> CommandPlan {
    match platform {
        Platform::Windows => powershell(WINDOWS_DELETE, name, false),
        Platform::MacOs => CommandPlan {
            program: "security".to_owned(),
            args: vec![
                "delete-generic-password".to_owned(),
                "-s".to_owned(),
                SERVICE.to_owned(),
                "-a".to_owned(),
                name.to_owned(),
            ],
            secret_on_stdin: false,
            secret_in_argv: false,
            env: Vec::new(),
            not_found_exit: 44,
        },
        Platform::Linux => CommandPlan {
            program: "secret-tool".to_owned(),
            args: vec![
                "clear".to_owned(),
                "service".to_owned(),
                SERVICE.to_owned(),
                "account".to_owned(),
                name.to_owned(),
            ],
            secret_on_stdin: false,
            secret_in_argv: false,
            env: Vec::new(),
            not_found_exit: 1,
        },
    }
}

/// 把一个计划变成实际进程并读回 stdout。退出码语义按平台翻译成 [`VaultError`]。
fn execute(plan: &CommandPlan, secret: Option<&Secret>) -> Result<String, VaultError> {
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    if plan.secret_on_stdin {
        command.stdin(Stdio::piped());
    } else {
        // 不给 stdin 留口子：外部工具若自行读 stdin 会立刻拿到 EOF，而不是挂住。
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => VaultError::BackendUnavailable(format!(
            "找不到 `{}`；装好它，或改设 FULU_VAULT_PASSPHRASE 走文件级加密后端",
            plan.program
        )),
        _ => VaultError::BackendUnavailable(format!("无法启动 `{}`：{error}", plan.program)),
    })?;

    if let Some(secret) = secret
        && let Some(mut stdin) = child.stdin.take()
    {
        use std::io::Write;
        stdin
            .write_all(secret.expose().as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| VaultError::Io(format!("写入 `{}` 的 stdin 失败：{e}", plan.program)))?;
        // 离开作用域即关闭 stdin，让脚本里的 `ReadToEnd` 返回。
    }

    let output = child
        .wait_with_output()
        .map_err(|e| VaultError::Io(format!("等待 `{}` 失败：{e}", plan.program)))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let code = output.status.code().unwrap_or(-1);
    if code == plan.not_found_exit {
        return Err(VaultError::NotFound(String::new()));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        format!("退出码 {code}")
    } else {
        format!("退出码 {code}：{detail}")
    };
    Err(VaultError::BackendUnavailable(format!(
        "`{}` 执行失败（{detail}）",
        plan.program
    )))
}

/// 去掉外部工具额外补的换行：密钥本身可以含空格，但两端换行是工具加的。
fn strip_trailing_newline(raw: &str) -> &str {
    raw.trim_end_matches(['\r', '\n'])
}

#[derive(Debug)]
pub struct SystemVault {
    platform: Platform,
    index: Option<PathBuf>,
}

impl SystemVault {
    /// 构造时不碰系统：只有 `get` / `put` / `delete` 才起进程。
    pub fn new(index: Option<PathBuf>) -> Self {
        Self {
            platform: current_platform(),
            index,
        }
    }

    /// 指定平台构造，供测试断言三条分支。
    pub fn for_platform(platform: Platform, index: Option<PathBuf>) -> Self {
        Self { platform, index }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    fn missing(&self, reference: &CredentialRef) -> VaultError {
        VaultError::NotFound(reference.as_str().to_owned())
    }
}

impl Vault for SystemVault {
    fn backend(&self) -> &'static str {
        system_backend_label(self.platform)
    }

    fn get(&self, reference: &CredentialRef) -> Result<Secret, VaultError> {
        let plan = plan_get(self.platform, reference.name());
        match execute(&plan, None) {
            Ok(stdout) => {
                let value = strip_trailing_newline(&stdout);
                if value.is_empty() {
                    // 退出码 0 却给了空串：当成「取不到」，绝不用空密钥继续。
                    return Err(self.missing(reference));
                }
                Ok(Secret::new(value))
            }
            Err(VaultError::NotFound(_)) => Err(self.missing(reference)),
            Err(other) => Err(other),
        }
    }

    fn put(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), VaultError> {
        if secret.is_empty() {
            return Err(VaultError::BackendUnavailable(
                "拒绝存入空密钥：空密钥等于关掉认证，不如不存".into(),
            ));
        }
        let plan = plan_put(self.platform, reference.name(), secret);
        execute(&plan, plan.secret_on_stdin.then_some(secret))?;
        if let Some(index) = &self.index {
            index_add(index, reference.as_str())?;
        }
        Ok(())
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), VaultError> {
        let plan = plan_delete(self.platform, reference.name());
        let result = execute(&plan, None);
        match result {
            Ok(_) => {
                if let Some(index) = &self.index {
                    index_remove(index, reference.as_str())?;
                }
                Ok(())
            }
            Err(VaultError::NotFound(_)) => Err(self.missing(reference)),
            Err(other) => Err(other),
        }
    }

    fn list(&self) -> Result<Vec<String>, VaultError> {
        // 系统库本身没有可移植的枚举接口；这里读的是只含引用名的索引。
        match &self.index {
            Some(path) => Ok(read_index(path)),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [Platform; 3] = [Platform::Windows, Platform::MacOs, Platform::Linux];

    #[test]
    fn secrets_never_reach_argv_except_on_macos() {
        let secret = Secret::new("sk-proj-topsecret");
        for platform in PLATFORMS {
            let plan = plan_put(platform, "cloud", &secret);
            let joined = plan.args.join(" ");
            if platform == Platform::MacOs {
                assert!(plan.secret_in_argv, "macOS 的 security 只能经 argv 收密钥");
                assert!(joined.contains("sk-proj-topsecret"));
            } else {
                assert!(!plan.secret_in_argv, "{platform:?} 不得把密钥放进 argv");
                assert!(
                    !joined.contains("sk-proj-topsecret"),
                    "{platform:?} 的 argv 泄漏了密钥：{joined}"
                );
                assert!(plan.secret_on_stdin, "{platform:?} 应改走 stdin");
            }
        }
    }

    #[test]
    fn reference_name_goes_through_env_on_windows_and_argv_elsewhere() {
        for platform in PLATFORMS {
            let plan = plan_get(platform, "cloud");
            let joined = plan.args.join(" ");
            assert!(!joined.contains("lu://"), "argv 里不该出现整条引用");
            if platform == Platform::Windows {
                assert!(
                    plan.args.iter().all(|a| !a.contains("cloud")),
                    "引用名不该拼进脚本"
                );
                assert_eq!(plan.env, vec![(USER_ENV.to_owned(), "cloud".to_owned())]);
            } else {
                assert!(joined.contains("cloud"), "{platform:?} 应把名字作为查询键");
            }
        }
    }

    #[test]
    fn every_platform_has_a_distinct_not_found_code() {
        assert_eq!(plan_get(Platform::Windows, "x").not_found_exit, 3);
        assert_eq!(plan_get(Platform::MacOs, "x").not_found_exit, 44);
        assert_eq!(plan_get(Platform::Linux, "x").not_found_exit, 1);
        for platform in PLATFORMS {
            assert_eq!(
                plan_get(platform, "x").not_found_exit,
                plan_delete(platform, "x").not_found_exit,
                "{platform:?} 的取/删应共用同一「不存在」退出码"
            );
        }
    }

    #[test]
    fn windows_scripts_are_constant_and_never_interpolate() {
        for script in [WINDOWS_GET, WINDOWS_PUT, WINDOWS_DELETE] {
            assert!(script.contains("$env:FULU_VAULT_USER"));
            assert!(!script.contains("lu://"));
            assert!(!script.contains("sk-"));
            // 脚本里不得出现任何格式化占位符
            assert!(!script.contains("{}"));
        }
        assert!(WINDOWS_PUT.contains("[Console]::In.ReadToEnd()"));
    }

    #[test]
    fn missing_tool_is_reported_as_backend_unavailable() {
        let plan = CommandPlan {
            program: "fulu-no-such-tool-9f3a".to_owned(),
            args: Vec::new(),
            secret_on_stdin: false,
            secret_in_argv: false,
            env: Vec::new(),
            not_found_exit: 3,
        };
        match execute(&plan, None) {
            Err(VaultError::BackendUnavailable(detail)) => {
                assert!(
                    detail.contains("FULU_VAULT_PASSPHRASE"),
                    "必须指路：{detail}"
                );
            }
            other => panic!("应报后端不可用，实际：{other:?}"),
        }
    }

    #[test]
    fn empty_secret_is_refused_before_touching_the_system() {
        let vault = SystemVault::for_platform(Platform::Linux, None);
        let reference = CredentialRef::parse("lu://default/cloud").expect("合法");
        match vault.put(&reference, &Secret::new("")) {
            Err(VaultError::BackendUnavailable(detail)) => assert!(detail.contains("空密钥")),
            other => panic!("应拒绝空密钥，实际：{other:?}"),
        }
    }

    #[test]
    fn newline_trimming_keeps_inner_spaces() {
        assert_eq!(strip_trailing_newline("a b c\r\n"), "a b c");
        assert_eq!(strip_trailing_newline("  padded  \n"), "  padded  ");
        assert_eq!(strip_trailing_newline("no-newline"), "no-newline");
        assert_eq!(strip_trailing_newline(""), "");
    }

    #[test]
    fn list_without_index_is_empty_not_an_error() {
        let vault = SystemVault::for_platform(Platform::Linux, None);
        assert!(vault.list().expect("应成功").is_empty());
        assert!(vault.backend().contains("Secret Service"));
    }
}

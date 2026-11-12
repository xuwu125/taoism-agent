//! 启动配置与二进制发现：全部是**纯逻辑**，不碰进程，便于单测锁住约束。

use crate::EngineError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// 唯一允许的监听地址。写死在代码里，不接受任何配置覆盖。
pub const LOOPBACK_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8080;
/// 显式指定二进制；设置但路径无效时**报错**，不静默回退到别处。
pub const BINARY_ENV: &str = "FULU_LLAMA_SERVER";
/// 运行记录路径覆盖（测试注入用）。
pub const RUN_RECORD_ENV: &str = "FULU_ENGINE_RUN_RECORD";

fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// `<平台数据目录>/taoism-agent`：模型、二进制、日志、运行记录都在这下面。
/// 与 `fulu_model::cache_dir()` 同源，避免另立一套目录约定。
pub fn app_dir() -> PathBuf {
    fulu_model::cache_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn bin_dir() -> PathBuf {
    app_dir().join("bin")
}
pub fn run_dir() -> PathBuf {
    app_dir().join("run")
}
pub fn log_dir() -> PathBuf {
    app_dir().join("logs")
}
pub fn default_log_path() -> PathBuf {
    log_dir().join("llama-server.log")
}
pub fn record_path() -> PathBuf {
    std::env::var_os(RUN_RECORD_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| run_dir().join("llama-server.json"))
}

/// 发现 `llama-server`，优先级固定且可审计：
/// 1. `FULU_LLAMA_SERVER`（显式指定，无效即报错，绝不静默回退）；
/// 2. 应用数据目录 `bin/llama-server[.exe]`，或 `bin/llama-b*/` 版本化目录中 build 号最大的；
/// 3. `PATH` 上的同名可执行文件。
///
/// **本函数绝不下载任何东西**：缺二进制就如实报缺失并给出放置位置。
pub fn discover_binary() -> Result<PathBuf, EngineError> {
    if let Some(explicit) = std::env::var_os(BINARY_ENV) {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(EngineError::BinaryNotFound(format!(
            "{BINARY_ENV}={} 指向的文件不存在（不回退到其它位置）",
            path.display()
        )));
    }

    let direct = bin_dir().join(exe_name());
    if direct.is_file() {
        return Ok(direct);
    }

    let mut versioned: Vec<(u64, PathBuf)> = std::fs::read_dir(bin_dir())
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 目录名形如 llama-b12314：按**数值**取最新，字典序会把 b9999 判得比 b12314 大。
            let build = name.strip_prefix("llama-b")?.parse::<u64>().ok()?;
            let exe = entry.path().join(exe_name());
            exe.is_file().then_some((build, exe))
        })
        .collect();
    versioned.sort_by_key(|(build, _)| std::cmp::Reverse(*build));
    if let Some((_, path)) = versioned.into_iter().next() {
        return Ok(path);
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe_name());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(EngineError::BinaryNotFound(format!(
        "未找到 {}。请设置 {BINARY_ENV}，或把官方预编译包解压到 {}（也可放进 PATH）",
        exe_name(),
        bin_dir().display()
    )))
}

/// `llama-server --version` 的实测结果。`meets_minimum = None` 表示**无法判定**
/// （版本串里没有可解析的 build 号），此时 CLI 必须显示 UNVERIFIED，不许当作通过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub version_line: String,
    pub build: Option<u64>,
    pub required_build: Option<u64>,
    pub meets_minimum: Option<bool>,
}

fn extract_build(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    if let Some(pos) = lower.find("build") {
        let digits: String = lower[pos + 5..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.len() >= 3 {
            return digits.parse().ok();
        }
    }
    // 清单 min_runtime 用 `b11960` 这种紧凑写法。
    let bytes = lower.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != b'b' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j - i >= 4 {
            return lower[i + 1..j].parse().ok();
        }
    }
    None
}

/// 启动前核对清单的 `min_runtime`（provider 注释里承诺过的断言，T22 落地）。
/// 低于门槛**硬拒绝**；解析不到则如实返回 `None`，由上层标 UNVERIFIED。
pub fn check_runtime(binary: &Path, min_runtime: &str) -> Result<RuntimeInfo, EngineError> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| EngineError::Config(format!("无法执行 --version：{e}")))?;
    if !output.status.success() {
        return Err(EngineError::Config(format!(
            "--version 返回 {:?}：{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version_line = text.lines().next().unwrap_or("").trim().to_owned();
    let build = extract_build(&text);
    let required_build = extract_build(min_runtime);
    let meets_minimum = match (build, required_build) {
        (Some(actual), Some(required)) => Some(actual >= required),
        _ => None,
    };
    if meets_minimum == Some(false) {
        return Err(EngineError::Config(format!(
            "运行时版本不满足清单 min_runtime「{min_runtime}」：实测 build {build:?} < {required_build:?}"
        )));
    }
    Ok(RuntimeInfo {
        version_line,
        build,
        required_build,
        meets_minimum,
    })
}

/// 一次拉起所需的全部参数。**不含任何凭据字段**：回环服务默认不启用 api-key，
/// 也不支持往命令行塞自由参数（避免绕过 host / port / model 约束）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub port: u16,
    pub context_tokens: u32,
    pub threads: Option<u32>,
    pub startup_timeout: Duration,
    pub poll_interval: Duration,
    /// `None` = 永不自动停；CLI 前台模式默认给一个有限值。
    pub idle_timeout: Option<Duration>,
    pub log_path: PathBuf,
}

impl EngineConfig {
    pub fn new(binary: PathBuf, model: PathBuf) -> Self {
        Self {
            binary,
            model,
            port: DEFAULT_PORT,
            context_tokens: 4096,
            threads: None,
            startup_timeout: Duration::from_secs(120),
            poll_interval: Duration::from_millis(250),
            idle_timeout: Some(Duration::from_secs(900)),
            log_path: default_log_path(),
        }
    }

    pub fn endpoint(&self) -> String {
        format!("http://{LOOPBACK_HOST}:{}", self.port)
    }

    /// 启动前闸门：文件存在性是唯一事实源，不做「看起来像」的猜测。
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.binary.is_file() {
            return Err(EngineError::BinaryNotFound(format!(
                "{} 不是可执行文件",
                self.binary.display()
            )));
        }
        if !self.model.is_file() {
            return Err(EngineError::Config(format!(
                "权重文件不存在：{}（先 `fulu model fetch` 或 `model import`）",
                self.model.display()
            )));
        }
        if self.port == 0 {
            return Err(EngineError::Config("端口不能为 0".into()));
        }
        if self.context_tokens == 0 {
            return Err(EngineError::Config("上下文长度必须大于 0".into()));
        }
        if self.threads == Some(0) {
            return Err(EngineError::Config("线程数必须大于 0".into()));
        }
        if self.startup_timeout.is_zero() || self.poll_interval.is_zero() {
            return Err(EngineError::Config("超时/轮询间隔必须大于 0".into()));
        }
        Ok(())
    }

    /// 参数白名单：`host` 恒为回环，端口/模型/上下文都来自已校验的配置。
    /// 用 `OsString` 保留非 UTF-8 路径，不做有损转换。
    pub fn args(&self) -> Vec<OsString> {
        let mut out = vec![
            OsString::from("-m"),
            self.model.as_os_str().to_os_string(),
            OsString::from("--host"),
            OsString::from(LOOPBACK_HOST),
            OsString::from("--port"),
            OsString::from(self.port.to_string()),
            OsString::from("-c"),
            OsString::from(self.context_tokens.to_string()),
        ];
        if let Some(threads) = self.threads {
            out.push(OsString::from("-t"));
            out.push(OsString::from(threads.to_string()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EngineConfig {
        let mut c = EngineConfig::new(PathBuf::from("srv.exe"), PathBuf::from("model.gguf"));
        c.binary = PathBuf::from("Cargo.toml"); // crate 根下确实存在的文件
        c.model = PathBuf::from("Cargo.toml");
        c.log_path = PathBuf::from("target/_engine_test.log");
        c
    }

    #[test]
    fn endpoint_is_always_loopback_http() {
        let mut c = cfg();
        c.port = 18080;
        assert_eq!(c.endpoint(), "http://127.0.0.1:18080");
    }

    #[test]
    fn args_pin_host_and_never_add_free_form_flags() {
        let mut c = cfg();
        c.port = 18080;
        c.context_tokens = 8192;
        c.threads = Some(4);
        let got: Vec<String> = c
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            got,
            vec![
                "-m",
                "Cargo.toml",
                "--host",
                "127.0.0.1",
                "--port",
                "18080",
                "-c",
                "8192",
                "-t",
                "4"
            ]
        );
        assert!(got.windows(2).any(|w| w == ["--host", "127.0.0.1"]));
    }

    #[test]
    fn validate_rejects_zero_port_and_context() {
        let mut c = cfg();
        c.port = 0;
        assert!(matches!(c.validate(), Err(EngineError::Config(_))));
        c.port = 8080;
        c.context_tokens = 0;
        assert!(matches!(c.validate(), Err(EngineError::Config(_))));
        c.context_tokens = 4096;
        c.threads = Some(0);
        assert!(matches!(c.validate(), Err(EngineError::Config(_))));
        c.threads = None;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_refuses_missing_binary_and_model() {
        let mut c = cfg();
        c.binary = PathBuf::from("definitely/not/here.exe");
        assert!(matches!(c.validate(), Err(EngineError::BinaryNotFound(_))));
        c.binary = PathBuf::from("Cargo.toml");
        c.model = PathBuf::from("definitely/not/here.gguf");
        assert!(matches!(c.validate(), Err(EngineError::Config(_))));
    }

    #[test]
    fn app_dir_stays_under_the_same_root_as_weights() {
        let weights = fulu_model::cache_dir();
        assert_eq!(weights.parent().unwrap(), app_dir());
        assert!(bin_dir().ends_with("bin"));
        assert!(run_dir().ends_with("run"));
        assert!(default_log_path().ends_with("llama-server.log"));
    }

    #[test]
    fn build_number_is_extracted_from_both_version_and_manifest_forms() {
        assert_eq!(
            extract_build("version: 0.9.0-dev (build 12314, commit abc)"),
            Some(12314)
        );
        assert_eq!(extract_build("llama.cpp >= b11960 (v0.8.0)"), Some(11960));
        assert_eq!(extract_build("version: 0.9.0"), None);
    }
}

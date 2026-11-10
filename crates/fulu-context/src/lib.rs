//! 会话头（Context）：每次会话启动采集一次的机器与环境信息（docs/02 第 8.8 节）。
//!
//! 三条红线：
//! 1. **只采无需系统授权的项**：不读摄像头/麦克风/精确位置，不采序列号与 MAC。
//! 2. **全项容错**：任何一项失败降级为「未知」并记入 issues，绝不阻塞会话启动。
//! 3. **只进会话头与缴符事件**：调用方（fulu-prompt 第 1 层 / fulu-audit）负责，
//!    本 crate 不写符文正文、不做任何持久化。
//!
//! IP 所在地通过委托平台 `curl` 获取（免 Rust TLS 依赖，M2.1 再评估 rustls），
//! 只取国家/城市/ISP 粗粒度；可关闭，失败静默降级，单次查询不重试轰炸。

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 单项采集超时。
const ITEM_TIMEOUT: Duration = Duration::from_secs(4);
const IP_TIMEOUT: Duration = Duration::from_secs(6);
pub const IP_ENDPOINT: &str = "https://ipwho.is/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub value: String,
    /// 来源（命令或环境），供会话头明示「采集来源与时间」。
    pub source: String,
}

impl Field {
    fn unknown(source: &str) -> Self {
        Self {
            value: "未知".into(),
            source: source.to_owned(),
        }
    }

    fn map_value(self, mapper: impl FnOnce(&str) -> String) -> Self {
        Self {
            value: mapper(&self.value),
            source: self.source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpLocation {
    pub ip: String,
    pub country: String,
    pub city: String,
    pub isp: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    pub os_name: Field,
    pub kernel: Field,
    pub arch: String,
    pub cpu_model: Field,
    pub cpu_cores: Field,
    pub mem_total: Field,
    pub disk_free: Field,
    pub timezone: Field,
    pub locale: Field,
    pub ip: Option<IpLocation>,
    /// 逐项失败原因；空 = 全部成功（含「按配置关闭」的说明）。
    pub issues: Vec<String>,
    /// 采集完成的本地时间。
    pub collected_at: String,
}

impl SessionContext {
    /// 供提示词第 1 层直接使用的紧凑摘要（每行一项，控制体积）。
    pub fn summary(&self) -> String {
        let mut lines = vec![
            format!(
                "系统：{} / {} / {}",
                self.os_name.value, self.kernel.value, self.arch
            ),
            format!(
                "CPU：{}（{} 核）",
                self.cpu_model.value, self.cpu_cores.value
            ),
            format!(
                "内存：{}；工作区可用磁盘：{}",
                self.mem_total.value, self.disk_free.value
            ),
            format!("时区/区域：{} / {}", self.timezone.value, self.locale.value),
        ];
        match &self.ip {
            Some(ip) => lines.push(format!(
                "IP 所在地：{} {}（{}，{}）",
                ip.country, ip.city, ip.isp, ip.ip
            )),
            None => lines.push("IP 所在地：未知（未查询或查询失败，已降级）".into()),
        }
        if !self.issues.is_empty() {
            lines.push(format!(
                "采集降级 {} 项：{}",
                self.issues.len(),
                self.issues.join("；")
            ));
        }
        lines.push(format!("采集时间：{}", self.collected_at));
        lines.join("\n")
    }
}

/// 采集入口。`workspace` 决定磁盘可用空间统计的卷（None 用当前目录）。
/// `include_ip = false` 时完全跳过网络查询并记录降级原因。
pub fn collect(workspace: Option<&Path>, include_ip: bool) -> SessionContext {
    let mut issues = Vec::new();
    let kv = collect_local(workspace, &mut issues);

    let field = |issues: &mut Vec<String>, key: &str, source: &str| -> Field {
        match kv.get(key).map(String::as_str) {
            Some(value) if !value.trim().is_empty() && value.trim() != "?" => Field {
                value: value.trim().to_owned(),
                source: source.to_owned(),
            },
            _ => {
                issues.push(format!("{key} 采集失败"));
                Field::unknown(source)
            }
        }
    };

    let windows = std::env::consts::OS == "windows";
    let os_name = if windows {
        field(
            &mut issues,
            "osname",
            "Get-CimInstance Win32_OperatingSystem",
        )
    } else {
        field(&mut issues, "osname", "uname")
    };
    let kernel = field(
        &mut issues,
        "kernel",
        if windows {
            "Win32_OperatingSystem.Version"
        } else {
            "uname -r"
        },
    );
    let cpu_model = field(&mut issues, "cpu", "平台命令");
    let cpu_cores = field(&mut issues, "cores", "平台命令");
    let mem_total = field(&mut issues, "mem", "平台命令").map_value(|raw| {
        raw.parse::<u64>()
            .ok()
            .map(human_bytes)
            .unwrap_or_else(|| raw.to_owned())
    });
    let disk_free = field(&mut issues, "diskfree", "平台命令").map_value(|raw| {
        raw.parse::<u64>()
            .ok()
            .map(human_bytes)
            .unwrap_or_else(|| raw.to_owned())
    });
    let timezone = field(&mut issues, "tz", "平台命令");
    let locale = field(&mut issues, "locale", "环境/平台命令");

    let ip = if include_ip {
        match resolve_ip() {
            Ok(ip) => Some(ip),
            Err(reason) => {
                issues.push(format!("IP 所在地查询失败：{reason}"));
                None
            }
        }
    } else {
        issues.push("IP 所在地已按配置关闭".into());
        None
    };

    SessionContext {
        os_name,
        kernel,
        arch: std::env::consts::ARCH.to_owned(),
        cpu_model,
        cpu_cores,
        mem_total,
        disk_free,
        timezone,
        locale,
        ip,
        issues,
        collected_at: local_now(),
    }
}

fn collect_local(workspace: Option<&Path>, issues: &mut Vec<String>) -> BTreeMap<String, String> {
    let dir: PathBuf = workspace
        .map(Into::into)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (program, args): (&str, Vec<String>) = match std::env::consts::OS {
        "windows" => (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                WINDOWS_SCRIPT.replace("{DIR}", &dir.to_string_lossy()),
            ],
        ),
        "macos" => (
            "sh",
            vec![
                "-c".into(),
                MACOS_SCRIPT.replace("{DIR}", &dir.to_string_lossy()),
            ],
        ),
        _ => (
            "sh",
            vec![
                "-c".into(),
                LINUX_SCRIPT.replace("{DIR}", &dir.to_string_lossy()),
            ],
        ),
    };
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match capture(program, &arg_refs, ITEM_TIMEOUT) {
        Ok(text) => parse_kv(&text),
        Err(reason) => {
            issues.push(format!("平台信息采集失败：{reason}"));
            BTreeMap::new()
        }
    }
}

const WINDOWS_SCRIPT: &str = r#"
$ErrorActionPreference='SilentlyContinue'
$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8
$os=Get-CimInstance Win32_OperatingSystem
$cpu=Get-CimInstance Win32_Processor | Select-Object -First 1
$cs=Get-CimInstance Win32_ComputerSystem
$ld=Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$((Get-Location).Drive.Name):'"
Write-Output "osname=$($os.Caption)"
Write-Output "kernel=$($os.Version)"
Write-Output "cpu=$($cpu.Name)"
Write-Output "cores=$($cpu.NumberOfLogicalProcessors)"
Write-Output "mem=$($cs.TotalPhysicalMemory)"
Write-Output "diskfree=$($ld.FreeSpace)"
Write-Output "tz=$([System.TimeZoneInfo]::Local.Id)"
Write-Output "locale=$([System.Globalization.CultureInfo]::CurrentCulture.Name)"
"#;

const LINUX_SCRIPT: &str = r#"
cd "{DIR}" 2>/dev/null
echo "osname=$(uname -o 2>/dev/null || uname -s)"
echo "kernel=$(uname -r)"
echo "cpu=$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | sed 's/^ *//')"
echo "cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo ?)"
echo "mem=$(awk '/MemTotal/ {print $2*1024; exit}' /proc/meminfo 2>/dev/null)"
echo "diskfree=$(df -Pk . 2>/dev/null | tail -1 | awk '{print $4*1024}')"
echo "tz=${TZ:-$(cat /etc/timezone 2>/dev/null)}"
echo "locale=${LANG:-${LC_ALL:-}}"
"#;

const MACOS_SCRIPT: &str = r#"
cd "{DIR}" 2>/dev/null
echo "osname=$(sw_vers -productName 2>/dev/null) $(sw_vers -productVersion 2>/dev/null)"
echo "kernel=$(uname -r)"
echo "cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null)"
echo "cores=$(sysctl -n hw.ncpu 2>/dev/null)"
echo "mem=$(sysctl -n hw.memsize 2>/dev/null)"
echo "diskfree=$(df -Pk . 2>/dev/null | tail -1 | awk '{print $4*1024}')"
echo "tz=$(readlink /etc/localtime 2>/dev/null | sed 's|.*/zoneinfo/||')"
echo "locale=${LANG:-${LC_ALL:-}}"
"#;

/// 带超时的子进程捕获：超时即 kill，绝不挂起会话。
fn capture(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 {program} 失败：{e}"))?;
    let pipe = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} 缺少 stdout"))?;
    let reader = std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("超过 {}ms，已终止", timeout.as_millis()));
            }
            None => std::thread::sleep(Duration::from_millis(30)),
        }
    }
    reader.join().map_err(|_| "读取线程异常".to_owned())
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

/// IP 所在地：委托平台 curl（免 Rust TLS 依赖），只取粗粒度并解析固定字段。
/// 失败返回 Err，由调用方降级；单次查询，不重试轰炸。
pub fn resolve_ip() -> Result<IpLocation, String> {
    let program = if cfg!(windows) { "curl.exe" } else { "curl" };
    let text = capture(
        program,
        &["-sS", "--max-time", "6", IP_ENDPOINT],
        IP_TIMEOUT,
    )?;
    parse_ipwho(&text).ok_or_else(|| "响应不是预期的 ipwho.is JSON".to_owned())
}

pub fn parse_ipwho(text: &str) -> Option<IpLocation> {
    let value: Value = serde_json::from_str(text).ok()?;
    let ip = value.get("ip")?.as_str()?.to_owned();
    let connection = value.get("connection")?;
    Some(IpLocation {
        ip,
        country: value
            .get("country")
            .and_then(Value::as_str)
            .unwrap_or("未知")
            .to_owned(),
        city: value
            .get("city")
            .and_then(Value::as_str)
            .unwrap_or("未知")
            .to_owned(),
        isp: connection
            .get("isp")
            .and_then(Value::as_str)
            .unwrap_or("未知")
            .to_owned(),
    })
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 5] = [
        ("B", 1),
        ("KB", 1024),
        ("MB", 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
        ("TB", 1024u64 * 1024 * 1024 * 1024),
    ];
    let mut chosen = UNITS[0];
    for unit in UNITS {
        if bytes >= unit.1 {
            chosen = unit;
        }
    }
    if chosen.1 == 1 {
        return format!("{bytes} B");
    }
    format!("{:.1} {}", bytes as f64 / chosen.1 as f64, chosen.0)
}

fn local_now() -> String {
    // 标准库没有本地时间格式化；用平台命令拿，失败降级为「未知时间」。
    let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile",
                "-Command",
                "Get-Date -Format 'yyyy-MM-dd HH:mm:ss'",
            ],
        )
    } else {
        ("date", vec!["+", "%Y-%m-%d %H:%M:%S"])
    };
    capture(program, &args, Duration::from_secs(2)).unwrap_or_else(|_| "未知时间".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_never_blocks_and_always_names_the_os() {
        let context = collect(None, false);
        assert!(!context.os_name.value.is_empty());
        assert!(!context.arch.is_empty());
        assert!(context.ip.is_none());
    }

    #[test]
    fn parse_ipwho_extracts_coarse_fields_only() {
        let json = r#"{"ip":"203.0.113.7","success":true,"country":"中国","city":"深圳",
            "connection":{"isp":"某运营商","asn":1234},"timezone":"Asia/Shanghai"}"#;
        let ip = parse_ipwho(json).unwrap();
        assert_eq!(ip.ip, "203.0.113.7");
        assert_eq!(ip.country, "中国");
        assert_eq!(ip.city, "深圳");
        assert_eq!(ip.isp, "某运营商");
        assert!(parse_ipwho("not json").is_none());
    }

    #[test]
    fn summary_keeps_compact_and_discloses_degradation() {
        let mut context = collect(None, false);
        context.issues.push("测试降级项".into());
        let summary = context.summary();
        assert!(summary.contains("IP 所在地：未知"));
        assert!(summary.contains("测试降级项"));
        assert!(summary.lines().count() <= 8, "会话头必须紧凑：\n{summary}");
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn parse_kv_survives_empty_lines() {
        let map = parse_kv("a=1\n\nb=2\nbroken\n");
        assert_eq!(map.get("a").map(String::as_str), Some("1"));
        assert_eq!(map.get("b").map(String::as_str), Some("2"));
        assert_eq!(map.len(), 2);
    }
}

//! 进程拉起、就绪探活、闲置判定与必然回收。

use crate::config::EngineConfig;
use crate::pidfile::RunRecord;
use crate::{EngineError, HealthState};
use fulu_provider::Endpoint;
use fulu_provider::http::{self, HttpRequest};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOG_TAIL_BYTES: u64 = 64 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// 启动前拒绝「端口上已经有服务」：宁可明确报错，也不悄悄把符文发给别人的进程。
pub fn assert_port_free(port: u16) -> Result<(), EngineError> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
        Ok(_) => Err(EngineError::PortInUse(port)),
        Err(_) => Ok(()),
    }
}

/// 单次 `/health` 探活。状态如实返回，**不把「连不上」伪装成「加载中」**。
pub fn probe_health(endpoint: &str, timeout: Duration) -> HealthState {
    let parsed = match Endpoint::parse(endpoint) {
        Ok(endpoint) => endpoint,
        Err(error) => return HealthState::Unreachable(error.to_string()),
    };
    match http::request(&parsed, "GET", "/health", None, timeout) {
        Ok(response) => match response.status {
            200 => HealthState::Ready {
                detail: response.body_text().unwrap_or_default(),
            },
            503 => HealthState::Loading {
                detail: "模型仍在加载或权重缺失".into(),
            },
            status => HealthState::HttpStatus(status),
        },
        Err(error) => HealthState::Unreachable(error.to_string()),
    }
}

/// 列出服务端已加载的模型。
///
/// 真实 `llama-server` b12314 的 `/v1/models` 返回 `{"models":[...]}`（字段是 `name` /
/// `model`），而 OpenAI 兼容格式是 `{"data":[{"id":...}]}`。**两者都认**，
/// 但都不存在时如实报错，不返回「空数组」冒充成功。
pub fn loaded_models(endpoint: &str, timeout: Duration) -> Result<Vec<String>, EngineError> {
    let parsed = Endpoint::parse(endpoint).map_err(|e| EngineError::Health(e.to_string()))?;
    let response = http::request(&parsed, "GET", "/v1/models", None, timeout)
        .map_err(|e| EngineError::Health(e.to_string()))?;
    if response.status != 200 {
        return Err(EngineError::Health(format!(
            "/v1/models 返回 HTTP {}",
            response.status
        )));
    }
    let text = response
        .body_text()
        .map_err(|e| EngineError::Health(e.to_string()))?;
    let root: Value =
        serde_json::from_str(&text).map_err(|e| EngineError::Health(format!("不是 JSON：{e}")))?;
    let items = root
        .get("data")
        .or_else(|| root.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| EngineError::Health("响应里既没有 data 也没有 models 数组".into()))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            ["id", "name", "model"]
                .iter()
                .find_map(|key| item.get(key).and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect())
}

impl HealthState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "READY",
            Self::Loading { .. } => "LOADING",
            Self::HttpStatus(_) => "HTTP_ERROR",
            Self::Unreachable(_) => "UNREACHABLE",
        }
    }
}

/// 纯函数：闲置是否到期。`touch()` 由调用方在**每次实际调用推理前**显式执行；
/// CLI 前台模式不代理请求，感知不到外部调用，这一点在文档里如实写明。
pub fn idle_expired(last_activity: Instant, idle_timeout: Option<Duration>) -> bool {
    idle_timeout.is_some_and(|limit| last_activity.elapsed() >= limit)
}

/// 读取日志尾部用于错误诊断。只读最后 64 KiB，绝不在内存里堆整份日志。
pub fn log_tail(path: &Path, lines: usize) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > LOG_TAIL_BYTES && file.seek(SeekFrom::End(-(LOG_TAIL_BYTES as i64))).is_err() {
        return String::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return String::new();
    }
    let collected: Vec<String> = BufReader::new(text.as_bytes())
        .lines()
        .map_while(Result::ok)
        .collect();
    let start = collected.len().saturating_sub(lines.max(1));
    collected[start..].join("\n")
}

/// Windows 上若父进程被作业对象（CI / 沙盒 / 终端管理器）托管，`--detach` 出来的
/// 子进程会随父进程一起被清理。尝试 `CREATE_BREAKAWAY_FROM_JOB` 脱离；作业不允许
/// 脱离时 CreateProcess 会失败，此时**显式回退**普通 spawn（不静默换语义）。
#[cfg(target_os = "windows")]
fn spawn_child(command: &mut Command, log_path: &Path) -> std::io::Result<Child> {
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW);
    match command.spawn() {
        Ok(child) => Ok(child),
        Err(_) => {
            // spawn 失败时 stdio 已被 take，回退前必须重新挂上日志，
            // 否则子进程输出会继承父进程管道，写满后卡死。
            command.creation_flags(0);
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)?;
            let stdout = log.try_clone()?;
            command.stdout(Stdio::from(stdout)).stderr(Stdio::from(log));
            command.spawn()
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn spawn_child(command: &mut Command, _log_path: &Path) -> std::io::Result<Child> {
    command.spawn()
}

fn kill_child(child: &mut Option<Child>) {
    if let Some(mut process) = child.take() {
        let _ = process.kill();
        let _ = process.wait(); // 回收句柄，避免僵尸进程
    }
}

/// 一次拉起的句柄。`Drop` 无条件回收**自己拉起的**进程（detach 过的不再回收）。
pub struct EngineHandle {
    child: Option<Child>,
    config: EngineConfig,
    endpoint: String,
    started_at: Instant,
    started_unix: u64,
    last_activity: Instant,
    owned: bool,
}

impl EngineHandle {
    /// 显式拉起。校验 → 端口占用检查 → spawn（stdout/stderr 全部重定向到日志文件，
    /// 否则管道缓冲区写满会把子进程卡死）→ 轮询 `/health` 直到就绪或超时。
    pub fn spawn(config: EngineConfig) -> Result<Self, EngineError> {
        config.validate()?;
        assert_port_free(config.port)?;
        if let Some(parent) = config.log_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| EngineError::Io(format!("{}: {e}", parent.display())))?;
        }
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&config.log_path)
            .map_err(|e| EngineError::Io(format!("{}: {e}", config.log_path.display())))?;
        let stdout = log
            .try_clone()
            .map_err(|e| EngineError::Io(e.to_string()))?;

        let mut command = Command::new(&config.binary);
        command
            .args(config.args())
            .current_dir(
                config
                    .binary
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new(".")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log));
        let launched_at = Instant::now();
        let launched_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut child = Some(
            spawn_child(&mut command, &config.log_path)
                .map_err(|e| EngineError::Spawn(format!("{}: {e}", config.binary.display())))?,
        );

        let endpoint = config.endpoint();
        let deadline = Instant::now() + config.startup_timeout;
        loop {
            if let Some(process) = child.as_mut()
                && let Ok(Some(status)) = process.try_wait()
            {
                kill_child(&mut child);
                return Err(EngineError::ExitedEarly {
                    code: status.code(),
                    log_tail: log_tail(&config.log_path, 20),
                });
            }
            if probe_health(&endpoint, HEALTH_TIMEOUT).is_ready() {
                break;
            }
            if Instant::now() >= deadline {
                kill_child(&mut child);
                return Err(EngineError::StartupTimeout {
                    seconds: config.startup_timeout.as_secs(),
                    log_tail: log_tail(&config.log_path, 20),
                });
            }
            thread::sleep(config.poll_interval);
        }

        let ready_at = Instant::now();
        Ok(Self {
            child,
            config,
            endpoint,
            started_at: launched_at,
            started_unix: launched_unix,
            last_activity: ready_at,
            owned: true,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn port(&self) -> u16 {
        self.config.port
    }
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
    pub fn started_at(&self) -> Instant {
        self.started_at
    }
    pub fn started_unix(&self) -> u64 {
        self.started_unix
    }

    pub fn health(&self) -> HealthState {
        probe_health(&self.endpoint, HEALTH_TIMEOUT)
    }
    pub fn models(&self) -> Result<Vec<String>, EngineError> {
        loaded_models(&self.endpoint, HEALTH_TIMEOUT)
    }

    /// 每次真实推理调用前显式记一次活动。
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }
    pub fn poll_idle(&self) -> bool {
        idle_expired(self.last_activity, self.config.idle_timeout)
    }
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(process) => matches!(process.try_wait(), Ok(None)),
            None => false,
        }
    }
    pub fn log_tail(&self, lines: usize) -> String {
        log_tail(&self.config.log_path, lines)
    }

    /// 转为后台服务：写运行记录后**放弃所有权**（进程继续，Drop 不再 kill）。
    /// 之后只能由 `fulu model stop` 回收，且 stop 会先核验端点与已加载模型。
    pub fn detach(&mut self) -> Result<RunRecord, EngineError> {
        let pid = self
            .pid()
            .ok_or_else(|| EngineError::Record("进程已退出，无法转入后台".into()))?;
        let record = RunRecord {
            pid,
            endpoint: self.endpoint.clone(),
            binary: self.config.binary.clone(),
            model: self.config.model.clone(),
            port: self.config.port,
            started_unix: self.started_unix,
        };
        crate::pidfile::write_record(&crate::config::record_path(), &record)?;
        self.owned = false;
        Ok(record)
    }

    /// 显式关停：kill + wait。幂等，重复调用无副作用。
    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        if self.owned {
            kill_child(&mut self.child);
            self.owned = false;
        }
        Ok(())
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // 正常退出（含前台模式等回车 / 闲置到期）必然回收；
        // Windows 上 Ctrl+C 默认终止进程、不跑 Drop，因此 CLI 明确提示用回车或 model stop。
        if self.owned {
            kill_child(&mut self.child);
        }
    }
}

/// 供上层把「服务端名号」与清单做一致性核验，避免连错模型。
pub fn request_once(
    endpoint: &str,
    path: &str,
    method: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<fulu_provider::HttpResponse, EngineError> {
    let parsed = Endpoint::parse(endpoint).map_err(|e| EngineError::Health(e.to_string()))?;
    http::send(
        &parsed,
        &HttpRequest {
            method,
            path,
            headers: &[],
            body,
            timeout,
        },
    )
    .map_err(|e| EngineError::Health(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn mock(status_line: &'static str, body: &'static str) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn health_ready_loading_and_bad_status_are_distinct() {
        let (port, _h) = mock("HTTP/1.1 200 OK", "{\"status\":\"ok\"}");
        let state = probe_health(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2));
        assert!(state.is_ready());
        assert_eq!(state.label(), "READY");

        let (port, _h) = mock(
            "HTTP/1.1 503 Service Unavailable",
            "{\"status\":\"loading\"}",
        );
        assert_eq!(
            probe_health(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)).label(),
            "LOADING"
        );

        let (port, _h) = mock("HTTP/1.1 404 Not Found", "nope");
        assert!(matches!(
            probe_health(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)),
            HealthState::HttpStatus(404)
        ));
    }

    #[test]
    fn non_loopback_endpoint_is_never_contacted() {
        assert_eq!(
            probe_health("http://example.com:8080", Duration::from_secs(1)).label(),
            "UNREACHABLE"
        );
    }

    #[test]
    fn models_accepts_openai_data_and_llama_cpp_models_shapes() {
        let (port, _h) = mock(
            "HTTP/1.1 200 OK",
            "{\"data\":[{\"id\":\"a\"},{\"id\":\"b\"}]}",
        );
        assert_eq!(
            loaded_models(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)).unwrap(),
            vec!["a", "b"]
        );

        let (port, _h) = mock(
            "HTTP/1.1 200 OK",
            "{\"models\":[{\"name\":\"C:/m/a.gguf\",\"model\":\"C:/m/a.gguf\"}]}",
        );
        assert_eq!(
            loaded_models(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)).unwrap(),
            vec!["C:/m/a.gguf"]
        );
    }

    #[test]
    fn models_rejects_unrecognised_payload() {
        let (port, _h) = mock("HTTP/1.1 200 OK", "{\"unexpected\":[]}");
        assert!(
            loaded_models(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)).is_err()
        );
    }

    #[test]
    fn occupied_port_is_refused_instead_of_reused() {
        // CI 修证（macos-15-intel 实测）：drop 后「刚释放的端口」正是内核优先
        // 复用的对象，并行的 mock（bind 127.0.0.1:0）可能立刻抢走它，
        // assert_port_free 的 connect 探测随即误报 PortInUse。语义本身
        // （占用 → 拒用；空闲 → 放行）不变，只是换新端口重试抢占比。
        for _ in 0..8 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            assert_eq!(assert_port_free(port), Err(EngineError::PortInUse(port)));
            drop(listener);
            // 释放后再次检查应通过；被并行测试抢占则换端口重来。
            if assert_port_free(port).is_ok() {
                return;
            }
        }
        panic!("连续 8 次刚释放的端口都被并行测试抢占——检查 mock 端口分配");
    }

    #[test]
    fn idle_expiry_is_explicit_and_disablable() {
        assert!(!idle_expired(Instant::now(), None));
        assert!(!idle_expired(Instant::now(), Some(Duration::from_secs(60))));
        assert!(idle_expired(
            Instant::now() - Duration::from_secs(10),
            Some(Duration::from_secs(1))
        ));
    }

    #[test]
    fn log_tail_only_returns_last_lines() {
        let path = std::env::temp_dir().join("fulu_engine_log_tail_test.log");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();
        assert_eq!(log_tail(&path, 2), "three\nfour");
        let _ = std::fs::remove_file(path);
    }
}

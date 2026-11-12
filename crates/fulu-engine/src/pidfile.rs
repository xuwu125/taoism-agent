//! 后台运行记录与**带核验的**停止。
//!
//! PID 会被操作系统复用，所以 `stop` 绝不「读个数字就 kill」：先确认端点仍是回环、
//! 仍可达，再确认端点上加载的模型与记录里的权重一致，全部通过才发终止信号。

use crate::process::{assert_port_free, loaded_models, probe_health};
use crate::{EngineError, HealthState};
use fulu_provider::Endpoint;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub pid: u32,
    pub endpoint: String,
    pub binary: PathBuf,
    pub model: PathBuf,
    pub port: u16,
    pub started_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    pub pid: u32,
    pub port: u16,
    pub port_released: bool,
    /// true = 停止时发现进程已不存在（清掉了陈旧记录），false = 本次真正发起了终止。
    pub already_stopped: bool,
}

/// 原子写回：先写 `.tmp` 再改名；旧记录存在时显式删除，避免 Windows 上
/// `rename` 覆盖已存在文件失败。
pub fn write_record(path: &Path, record: &RunRecord) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| EngineError::Record(format!("{}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(record)
        .map_err(|e| EngineError::Record(format!("序列化失败：{e}")))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| EngineError::Record(e.to_string()))?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| EngineError::Record(e.to_string()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| EngineError::Record(e.to_string()))?;
    Ok(())
}

/// 不存在 = 没有后台服务（Ok(None)）；**损坏 = 明确报错**，绝不静默当成「没在运行」。
pub fn read_record(path: &Path) -> Result<Option<RunRecord>, EngineError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| EngineError::Record(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text).map(Some).map_err(|e| {
        EngineError::Record(format!(
            "{} 内容损坏：{e}；不静默忽略，请人工核对后删除",
            path.display()
        ))
    })
}

pub fn remove_record(path: &Path) -> Result<(), EngineError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::Record(error.to_string())),
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// 停止前的双向核验。任何一项不通过都拒绝终止，把误杀风险挡在发信号之前。
pub fn verify_owned_service(record: &RunRecord) -> Result<(), EngineError> {
    // ① 端点必须仍是回环：记录若被改写成公网地址，我们连请求都不会发。
    Endpoint::parse(&record.endpoint)
        .map_err(|e| EngineError::Stop(format!("运行记录端点不合法：{e}")))?;

    // ② 服务必须还活着且不是「别人占了这个端口还返回奇怪状态」。
    match probe_health(&record.endpoint, Duration::from_secs(2)) {
        HealthState::Ready { .. } | HealthState::Loading { .. } => {}
        HealthState::Unreachable(msg) => {
            return Err(EngineError::Stop(format!(
                "端点不可达（{msg}）；拒绝按 PID 盲杀，请人工确认后清理记录"
            )));
        }
        HealthState::HttpStatus(status) => {
            return Err(EngineError::Stop(format!(
                "/health 返回 HTTP {status}；无法确认是本项目的服务，拒绝终止"
            )));
        }
    }

    // ③ 已加载模型必须与记录里的权重一致（llama.cpp 返回绝对路径，分隔符可能不同）。
    let models = loaded_models(&record.endpoint, Duration::from_secs(2))?;
    let expected = normalize_path(&record.model);
    let expected_name = record
        .model
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase());
    let matched = models.iter().any(|loaded| {
        let normalized = normalize_path(Path::new(loaded));
        normalized == expected
            || expected_name
                .as_ref()
                .is_some_and(|name| normalized.ends_with(name))
    });
    if !matched {
        return Err(EngineError::Stop(format!(
            "端点上加载的模型与运行记录不一致（记录：{}；实际：{:?}）；拒绝终止，防止误杀别人的服务",
            record.model.display(),
            models
        )));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn terminate_pid(pid: u32) -> Result<(), EngineError> {
    // 不带 /T：只终止目标进程，避免连带杀掉无关的进程树。
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output()
        .map_err(|e| EngineError::Stop(format!("调用 taskkill 失败：{e}")))?;
    if !output.status.success() {
        return Err(EngineError::Stop(format!(
            "taskkill 返回 {:?}：{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn terminate_pid(pid: u32) -> Result<(), EngineError> {
    let output = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .map_err(|e| EngineError::Stop(format!("调用 kill 失败：{e}")))?;
    if !output.status.success() {
        return Err(EngineError::Stop(format!(
            "kill -TERM 返回 {:?}：{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// 端点不可达但 PID 已死：服务早已停止，清记录即可，不触发「拒绝盲杀」。
/// PID 活着才需要端点核验——那是防误杀的闸门，不是清理残留的障碍。
#[cfg(target_os = "windows")]
pub fn pid_alive(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        }
        _ => true, // 查询失败时宁可保守当作活着，走完整核验路径。
    }
}

#[cfg(not(target_os = "windows"))]
pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(true) // 查询失败时保守当作活着。
}

/// 核验 → 终止 → 等端口释放 → 删除记录。端口没释放也如实回报，不谎称干净。
pub fn stop_owned_service(record: &RunRecord, path: &Path) -> Result<StopReport, EngineError> {
    if !pid_alive(record.pid) {
        // 进程已不存在：没有可误杀的对象，直接清掉陈旧记录。
        let released = assert_port_free(record.port).is_ok();
        remove_record(path)?;
        return Ok(StopReport {
            pid: record.pid,
            port: record.port,
            port_released: released,
            already_stopped: true,
        });
    }
    verify_owned_service(record)?;
    terminate_pid(record.pid)?;
    let mut released = false;
    for _ in 0..30 {
        if assert_port_free(record.port).is_ok() {
            released = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    remove_record(path)?;
    Ok(StopReport {
        pid: record.pid,
        port: record.port,
        port_released: released,
        already_stopped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_record_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fulu_engine_record_{tag}_{}.json",
            std::process::id()
        ))
    }

    fn sample() -> RunRecord {
        RunRecord {
            pid: 4242,
            endpoint: "http://127.0.0.1:18080".into(),
            binary: PathBuf::from("C:/bin/llama-server.exe"),
            model: PathBuf::from("C:/models/default/model.gguf"),
            port: 18080,
            started_unix: 1_700_000_000,
        }
    }

    #[test]
    fn record_roundtrip_and_removal() {
        let path = temp_record_path("roundtrip");
        remove_record(&path).unwrap();
        assert_eq!(read_record(&path).unwrap(), None);
        write_record(&path, &sample()).unwrap();
        assert_eq!(read_record(&path).unwrap(), Some(sample()));
        remove_record(&path).unwrap();
        assert_eq!(read_record(&path).unwrap(), None);
    }

    #[test]
    fn corrupted_record_is_reported_not_ignored() {
        let path = temp_record_path("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            read_record(&path),
            Err(EngineError::Record(msg)) if msg.contains("损坏")
        ));
        remove_record(&path).unwrap();
    }

    #[test]
    fn non_loopback_record_is_rejected_before_any_request() {
        let mut record = sample();
        record.endpoint = "http://example.com:8080".into();
        let error = verify_owned_service(&record).unwrap_err();
        assert!(error.to_string().contains("端点不合法"));
    }

    /// 按请求路径返回不同响应的最小回环 HTTP mock。
    fn mock_service(loaded: &'static str) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 2048];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]);
                let body = if request.starts_with("GET /health") {
                    "{\"status\":\"ok\"}".to_owned()
                } else {
                    format!("{{\"models\":[{{\"name\":\"{loaded}\",\"model\":\"{loaded}\"}}]}}")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn matching_loaded_model_passes_verification() {
        let (port, _h) = mock_service("C:/models/default/model.gguf");
        let mut record = sample();
        record.endpoint = format!("http://127.0.0.1:{port}");
        record.port = port;
        assert!(verify_owned_service(&record).is_ok());
    }

    #[test]
    fn mismatched_loaded_model_blocks_termination() {
        let (port, _h) = mock_service("C:/models/other/other.gguf");
        let mut record = sample();
        record.endpoint = format!("http://127.0.0.1:{port}");
        record.port = port;
        let error = verify_owned_service(&record).unwrap_err();
        assert!(error.to_string().contains("不一致"));
    }

    #[test]
    fn windows_style_paths_match_forward_slash_service_paths() {
        let (port, _h) = mock_service("C:/models/default/model.gguf");
        let mut record = sample();
        record.model = PathBuf::from(r"C:\models\default\model.gguf");
        record.endpoint = format!("http://127.0.0.1:{port}");
        record.port = port;
        assert!(verify_owned_service(&record).is_ok());
    }

    #[test]
    fn stop_never_leaks_credentials_into_debug() {
        // 记录里没有密钥字段；确认 Debug 输出只含可公开的路径信息。
        let debug = format!("{:?}", sample());
        assert!(debug.contains("model.gguf"));
        assert!(!debug.to_lowercase().contains("api_key"));
    }

    #[test]
    fn dead_pid_cleans_stale_record_without_killing_or_verifying() {
        // 端点不可达 + PID 已死 = 陈旧记录，应直接清理而不是拒绝。
        let mut child = if cfg!(windows) {
            Command::new("cmd")
                .args(["/C", "exit", "0"])
                .spawn()
                .unwrap()
        } else {
            Command::new("true").spawn().unwrap()
        };
        let dead_pid = child.id();
        let _ = child.wait();

        let path = temp_record_path("stale");
        let mut record = sample();
        record.pid = dead_pid;
        // 端口 1 几乎不可能被监听：若真有服务占着，报告会如实说未释放。
        record.endpoint = "http://127.0.0.1:1".into();
        record.port = 1;
        let report = stop_owned_service(&record, &path).unwrap();
        assert!(report.already_stopped);
        assert_eq!(read_record(&path).unwrap(), None);
    }
}

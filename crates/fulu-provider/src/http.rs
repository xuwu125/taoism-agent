//! 极小 HTTP/1.1 客户端，**只服务回环地址上的 `llama-server`**。
//!
//! 设计取舍（docs/02 第 8.5 节）：M2 只需要「向本机 POST 一段 JSON、读回一段 JSON」，
//! 因此不引入 reqwest / tokio / TLS 栈。用 `std::net::TcpStream` 手写最小实现，
//! 依赖为零、许可为零、读写超时可精确控制，也不给「符箓台」增加常驻内存。
//!
//! **职责边界**：不支持 HTTPS、不支持重定向、不支持公网主机。模型权重的下载
//! 属于另一条链路，不经过这里。

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 单次响应上限：防止本地服务异常时把进程撑爆。
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    InvalidEndpoint(String),
    Connect(String),
    Io(String),
    Timeout,
    MalformedResponse(String),
    BodyTooLarge(usize),
    HttpStatus(u16, String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(msg) => write!(f, "非法的本地推理服务地址：{msg}"),
            Self::Connect(msg) => write!(f, "无法连接本地推理服务：{msg}"),
            Self::Io(msg) => write!(f, "本地推理服务 I/O 失败：{msg}"),
            Self::Timeout => write!(f, "本地推理服务读写超时"),
            Self::MalformedResponse(msg) => write!(f, "本地推理服务响应格式错误：{msg}"),
            Self::BodyTooLarge(size) => write!(f, "响应体超过上限：{size} bytes"),
            Self::HttpStatus(code, body) => {
                write!(f, "本地推理服务返回 HTTP {code}：{}", truncate(body, 200))
            }
        }
    }
}

impl std::error::Error for HttpError {}

fn truncate(text: &str, max: usize) -> String {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    if text.len() > end {
        out.push('…');
    }
    out
}

fn io_error(error: std::io::Error) -> HttpError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => HttpError::Timeout,
        _ => HttpError::Io(error.to_string()),
    }
}

/// 只允许 `http://127.0.0.1:<port>` / `http://localhost:<port>`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn parse(url: &str) -> Result<Self, HttpError> {
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            HttpError::InvalidEndpoint("只允许明文 HTTP 回环地址，不接受 HTTPS 或裸地址".into())
        })?;
        let authority = rest.split('/').next().unwrap_or(rest);
        // 先判主机再判端口：否则 `http://example.com/mcp` 会报「缺少端口」，
        // 把「不能发往网络」这个真正的原因盖掉。
        let (host, port_text) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if host != "127.0.0.1" && host != "localhost" {
            return Err(HttpError::InvalidEndpoint(format!(
                "主机 {host} 不是回环地址，禁止把流量发往网络"
            )));
        }
        let Some(port_text) = port_text else {
            return Err(HttpError::InvalidEndpoint("缺少端口".into()));
        };
        let port: u16 = port_text
            .parse()
            .map_err(|_| HttpError::InvalidEndpoint(format!("端口不合法：{port_text}")))?;
        if port == 0 {
            return Err(HttpError::InvalidEndpoint("端口不能为 0".into()));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// 响应头原文（键统一小写）。MCP 的 `mcp-session-id` 依赖它，故不能只留 body。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn body_text(&self) -> Result<String, HttpError> {
        String::from_utf8(self.body.clone())
            .map_err(|e| HttpError::MalformedResponse(format!("响应不是 UTF-8：{e}")))
    }

    /// 按小写键取响应头（多值取第一个）。
    pub fn header(&self, name: &str) -> Option<&str> {
        let lowered = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == lowered)
            .map(|(_, value)| value.as_str())
    }

    /// 非 2xx 一律视为错误，不吞掉服务端的报错正文。
    pub fn require_success(&self) -> Result<(), HttpError> {
        if (200..300).contains(&self.status) {
            Ok(())
        } else {
            Err(HttpError::HttpStatus(
                self.status,
                String::from_utf8_lossy(&self.body).into_owned(),
            ))
        }
    }
}

fn read_line<R: BufRead>(reader: &mut R, line: &mut String) -> Result<usize, HttpError> {
    reader.read_line(line).map_err(io_error)
}

fn parse_status_line(line: &str) -> Result<u16, HttpError> {
    let mut parts = line.split_whitespace();
    let _version = parts
        .next()
        .ok_or_else(|| HttpError::MalformedResponse("缺少状态行".into()))?;
    let code = parts
        .next()
        .ok_or_else(|| HttpError::MalformedResponse("状态行缺少状态码".into()))?;
    code.parse()
        .map_err(|_| HttpError::MalformedResponse(format!("状态码不合法：{code}")))
}

fn check_limit(size: usize) -> Result<(), HttpError> {
    if size > MAX_RESPONSE_BYTES {
        Err(HttpError::BodyTooLarge(size))
    } else {
        Ok(())
    }
}

fn read_fixed<R: BufRead>(reader: &mut R, length: usize) -> Result<Vec<u8>, HttpError> {
    check_limit(length)?;
    let mut buffer = Vec::with_capacity(length);
    let read = reader
        .take(length as u64)
        .read_to_end(&mut buffer)
        .map_err(io_error)?;
    if read != length {
        return Err(HttpError::MalformedResponse(format!(
            "响应体被截断：声明 {length} 字节，实收 {read} 字节"
        )));
    }
    Ok(buffer)
}

fn read_to_close<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, HttpError> {
    let mut buffer = Vec::new();
    reader
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut buffer)
        .map_err(io_error)?;
    check_limit(buffer.len())?;
    Ok(buffer)
}

fn read_chunked<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, HttpError> {
    let mut output = Vec::new();
    loop {
        let mut size_line = String::new();
        if read_line(reader, &mut size_line)? == 0 {
            return Err(HttpError::MalformedResponse("chunked 响应意外结束".into()));
        }
        let size_text = size_line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| HttpError::MalformedResponse(format!("chunk 长度不合法：{size_text}")))?;
        if size == 0 {
            // 跳过 trailer，直到空行。
            let mut trailer = String::new();
            let _ = read_line(reader, &mut trailer)?;
            break;
        }
        check_limit(output.len().saturating_add(size))?;
        let mut chunk = vec![0_u8; size];
        reader.read_exact(&mut chunk).map_err(io_error)?;
        output.extend_from_slice(&chunk);
        let mut crlf = [0_u8; 2];
        reader.read_exact(&mut crlf).map_err(io_error)?;
    }
    Ok(output)
}

/// 一次请求的全部要素。`headers` 用于外宗（MCP）这类需要认证头的明文回环服务；
/// 值只在内存里用，不写日志、不进缴符。
#[derive(Debug, Clone)]
pub struct HttpRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
    pub timeout: Duration,
}

impl<'a> HttpRequest<'a> {
    pub fn get(path: &'a str, timeout: Duration) -> Self {
        Self {
            method: "GET",
            path,
            headers: &[],
            body: None,
            timeout,
        }
    }

    pub fn post(path: &'a str, body: &'a [u8], timeout: Duration) -> Self {
        Self {
            method: "POST",
            path,
            headers: &[],
            body: Some(body),
            timeout,
        }
    }

    #[must_use]
    pub fn with_headers(mut self, headers: &'a [(&'a str, &'a str)]) -> Self {
        self.headers = headers;
        self
    }
}

/// 发一次请求并读完整响应。每次新建连接（`Connection: close`），避免连接复用状态。
pub fn send(endpoint: &Endpoint, request: &HttpRequest<'_>) -> Result<HttpResponse, HttpError> {
    let address = format!("{}:{}", endpoint.host, endpoint.port);
    let socket = address
        .to_socket_addrs()
        .map_err(|e| HttpError::Connect(e.to_string()))?
        .next()
        .ok_or_else(|| HttpError::Connect(format!("无法解析 {address}")))?;
    let mut stream = TcpStream::connect_timeout(&socket, request.timeout).map_err(io_error)?;
    stream
        .set_read_timeout(Some(request.timeout))
        .map_err(io_error)?;
    stream
        .set_write_timeout(Some(request.timeout))
        .map_err(io_error)?;

    let payload = request.body.unwrap_or(&[]);
    let mut header = format!(
        "{} {} HTTP/1.1\r\nHost: {address}\r\nUser-Agent: taoism-agent/fulu\r\nAccept: application/json\r\nConnection: close\r\n",
        request.method, request.path
    );
    for (name, value) in request.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }
    if !payload.is_empty() {
        header.push_str("Content-Type: application/json\r\n");
    }
    header.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(payload))
        .and_then(|_| stream.flush())
        .map_err(io_error)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if read_line(&mut reader, &mut line)? == 0 {
        return Err(HttpError::MalformedResponse("响应为空".into()));
    }
    let status = parse_status_line(&line)?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        line.clear();
        let read = read_line(&mut reader, &mut line)?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(HttpError::MalformedResponse(format!(
                "响应头缺少冒号：{}",
                truncate(line.trim(), 120)
            )));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        match key.as_str() {
            "content-length" => {
                content_length = value.parse().ok();
            }
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                chunked = true;
            }
            _ => {}
        }
        headers.push((key, value));
    }

    let body_bytes = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(length) = content_length {
        read_fixed(&mut reader, length)?
    } else {
        read_to_close(&mut reader)?
    };

    Ok(HttpResponse {
        status,
        headers,
        body: body_bytes,
    })
}

/// 兼容入口：无自定义请求头的简单调用（provider 自身用）。
pub fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    send(
        endpoint,
        &HttpRequest {
            method,
            path,
            headers: &[],
            body,
            timeout,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn serve_once(raw: String) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(raw.as_bytes());
        });
        (port, handle)
    }

    #[test]
    fn endpoint_only_accepts_loopback_http() {
        assert_eq!(Endpoint::parse("http://127.0.0.1:8080").unwrap().port, 8080);
        assert_eq!(
            Endpoint::parse("http://localhost:9000/v1").unwrap().host,
            "localhost"
        );
        assert!(Endpoint::parse("http://0.0.0.0:8080").is_err());
        assert!(Endpoint::parse("https://127.0.0.1:8080").is_err());
        assert!(Endpoint::parse("http://api.example.com:80").is_err());
        assert!(Endpoint::parse("http://127.0.0.1:0").is_err());
    }

    #[test]
    fn reads_content_length_response() {
        let body = "{\"status\":\"ok\"}";
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (port, handle) = serve_once(raw);
        let endpoint = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let response = request(&endpoint, "GET", "/health", None, Duration::from_secs(2)).unwrap();
        handle.join().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text().unwrap(), body);
        response.require_success().unwrap();
    }

    #[test]
    fn reads_chunked_response() {
        let first = "{\"choices\":[{\"message\":";
        let second = "{\"content\":\"你好\"}}]}";
        let raw = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n{first}\r\n{:X}\r\n{second}\r\n0\r\n\r\n",
            first.len(),
            second.len()
        );
        let (port, handle) = serve_once(raw);
        let endpoint = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let response = request(
            &endpoint,
            "POST",
            "/v1/chat/completions",
            Some(b"{}"),
            Duration::from_secs(2),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(response.body_text().unwrap(), format!("{first}{second}"));
    }

    #[test]
    fn non_2xx_is_not_swallowed() {
        let body = "model loading";
        let raw = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (port, handle) = serve_once(raw.to_owned());
        let endpoint = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let response = request(&endpoint, "GET", "/health", None, Duration::from_secs(2)).unwrap();
        handle.join().unwrap();
        assert!(matches!(
            response.require_success(),
            Err(HttpError::HttpStatus(503, _))
        ));
    }
}

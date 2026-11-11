//! HTTPS/1.1 GET 的手写实现：rustls 握手 + 流式响应体 + sha256。
//!
//! 与 `fulu-provider::http`（明文回环）是两条独立链路：这里只服务模型下载，
//! 不承担任何推理 / MCP 流量，也不反向依赖 provider crate。

use crate::url::{HTTPS_DEFAULT_PORT, HttpsUrl};
use rustls_pki_types::ServerName;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 单行请求/响应头的长度上限：防止对端用超长行撑爆内存。
const MAX_HEADER_LINE: usize = 16 * 1024;
/// 重定向 Location 头长度上限。
const MAX_LOCATION: usize = 2048;
/// 流式读取的缓冲块大小。
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    InvalidUrl(String),
    Connect(String),
    Proxy(String),
    Tls(String),
    Io(String),
    Timeout,
    Protocol(String),
    HttpStatus(u16, String),
    RedirectTooMany(String),
    RedirectInsecure(String),
    BodyTooLarge { limit: u64, received: u64 },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(msg) => write!(f, "非法的下载地址：{msg}"),
            Self::Connect(msg) => write!(f, "无法连接下载服务：{msg}"),
            Self::Proxy(msg) => write!(f, "代理握手失败：{msg}"),
            Self::Tls(msg) => write!(f, "TLS 握手/校验失败：{msg}"),
            Self::Io(msg) => write!(f, "下载 I/O 失败：{msg}"),
            Self::Timeout => write!(f, "下载读写超时"),
            Self::Protocol(msg) => write!(f, "HTTP 协议错误：{msg}"),
            Self::HttpStatus(code, body) => {
                write!(f, "下载服务返回 HTTP {code}：{}", truncate(body, 200))
            }
            Self::RedirectTooMany(msg) => write!(f, "重定向次数超限：{msg}"),
            Self::RedirectInsecure(msg) => write!(f, "重定向被拒绝（降级明文 HTTP）：{msg}"),
            Self::BodyTooLarge { limit, received } => {
                write!(f, "响应体超过上限 {limit} 字节（实收 {received}）")
            }
        }
    }
}

impl std::error::Error for DownloadError {}

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

fn io_error(error: std::io::Error) -> DownloadError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => DownloadError::Timeout,
        _ => DownloadError::Io(error.to_string()),
    }
}

/// rustls 对「对端关闭连接但没发 close_notify」返回 UnexpectedEof；公网服务器
/// 这么关很常见。只要我们不是在半截记录里（完整性由 Content-Length / chunked
/// 终止标记 / sha256 校验兜底），就按干净 EOF 处理，给上层 Ok(0)。
fn close_notify_eof(error: &std::io::Error) -> bool {
    error.to_string().contains("close_notify")
}

/// 读一块数据；无 close_notify 的对端关闭按 EOF（Ok(0)）处理。
///
/// rustls 可能在报 UnexpectedEof 的那次调用里仍有明文留在内部缓冲（调用返回 Err
/// 但数据没有丢），所以先重试一次把剩余数据拿回来，再错/空才视为 EOF。
fn read_chunk_or_eof(
    reader: &mut BufReader<TlsStream>,
    buffer: &mut [u8],
) -> Result<usize, DownloadError> {
    match reader.read(buffer) {
        Ok(read) => Ok(read),
        Err(error) if close_notify_eof(&error) => match reader.read(buffer) {
            Ok(read) => Ok(read),
            Err(_) => Ok(0),
        },
        Err(error) => Err(io_error(error)),
    }
}

/// 按行读取（`read_line` 到 `\n` 为止，**绝不**用 `read_to_string`——那会把
/// 整个流当一行吞掉）。close_notify EOF 先重试一次拿缓冲剩余数据，再错按 EOF。
fn read_line_or_eof<R: BufRead>(reader: &mut R, line: &mut String) -> Result<usize, DownloadError> {
    match reader.read_line(line) {
        Ok(_read) if line.len() > MAX_HEADER_LINE => {
            Err(DownloadError::Protocol("响应头单行超长".into()))
        }
        Ok(read) => Ok(read),
        Err(error) if close_notify_eof(&error) => match reader.read_line(line) {
            Ok(_read) if line.len() > MAX_HEADER_LINE => {
                Err(DownloadError::Protocol("响应头单行超长".into()))
            }
            Ok(read) => Ok(read),
            Err(_) => Ok(0),
        },
        Err(error) => Err(io_error(error)),
    }
}

/// HTTP 明文代理（CONNECT 隧道）。只支持 `http://host:port`——代理自身走明文，
/// 模型流量的 TLS 在隧道内端到端，代理无法窥探内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyAddr {
    pub host: String,
    pub port: u16,
}

impl ProxyAddr {
    pub fn parse(raw: &str) -> Result<Self, DownloadError> {
        let rest = raw.strip_prefix("http://").ok_or_else(|| {
            DownloadError::Proxy(format!("代理地址只接受 http:// 前缀，实际输入：{raw}"))
        })?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let (host, port_text) = authority
            .rsplit_once(':')
            .ok_or_else(|| DownloadError::Proxy(format!("代理地址缺少端口：{raw}")))?;
        let port: u16 = port_text
            .parse()
            .map_err(|_| DownloadError::Proxy(format!("代理端口不合法：{port_text}")))?;
        if port == 0 || host.is_empty() {
            return Err(DownloadError::Proxy(format!("代理地址不合法：{raw}")));
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DownloadConfig {
    pub proxy: Option<ProxyAddr>,
    /// 重定向跟随上限（不含首次请求）。
    pub max_redirects: usize,
    pub connect_timeout: Duration,
    /// 单次读取的空闲超时（不是总时长）：慢速大文件下载只要持续有字节就不算超时。
    pub read_timeout: Duration,
    /// 响应体上限；权重下载传 None（上限由清单 bytes 在调用方校验）。
    pub max_body: Option<u64>,
    /// 测试钩子：额外信任的自签根证书（DER 编码）。生产恒为 None——信任锚固定为
    /// webpki-roots 内嵌的 Mozilla 根集，绝不静默放宽。
    pub trust_root_der: Option<Vec<u8>>,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            max_redirects: 5,
            connect_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            max_body: None,
            trust_root_der: None,
        }
    }
}

/// 收到最终响应头（重定向已在内部消化）后、读响应体前的一次回调。
/// 断点续传用它区分 206（续传）与 200（服务器忽略 Range，须从头重来）。
#[derive(Debug, Clone)]
pub struct ResponseHead {
    pub status: u16,
    pub content_length: Option<u64>,
    pub headers: Vec<(String, String)>,
    pub final_url: String,
    /// 每一跳重定向落点（按顺序），便于审计「到底连过谁」。
    pub redirect_hops: Vec<String>,
}

impl ResponseHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lowered = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == lowered)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub status: u16,
    pub final_url: String,
    pub bytes: u64,
    pub sha256: String,
    pub redirect_hops: Vec<String>,
}

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn client_config(config: &DownloadConfig) -> Result<rustls::ClientConfig, DownloadError> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| DownloadError::Tls(e.to_string()))?;
    let mut roots = rustls::RootCertStore::empty();
    if let Some(root) = &config.trust_root_der {
        roots
            .add(rustls_pki_types::CertificateDer::from(root.clone()))
            .map_err(|e| DownloadError::Tls(format!("无法装载测试根证书：{e}")))?;
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    Ok(builder.with_root_certificates(roots).with_no_client_auth())
}

fn server_name(host: &str) -> Result<ServerName<'static>, DownloadError> {
    ServerName::try_from(host.to_owned())
        .map_err(|_| DownloadError::InvalidUrl(format!("主机名无法用于 TLS 校验：{host}")))
}

fn tcp_connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, DownloadError> {
    let address = format!("{host}:{port}");
    let socket = address
        .to_socket_addrs()
        .map_err(|e| DownloadError::Connect(format!("解析 {address} 失败：{e}")))?
        .next()
        .ok_or_else(|| DownloadError::Connect(format!("无法解析 {address}")))?;
    let stream = TcpStream::connect_timeout(&socket, timeout).map_err(io_error)?;
    stream.set_read_timeout(Some(timeout)).map_err(io_error)?;
    stream.set_write_timeout(Some(timeout)).map_err(io_error)?;
    Ok(stream)
}

/// 与明文代理完成 CONNECT 隧道握手，返回已打通的原始 TCP 流。
fn connect_via_proxy(
    proxy: &ProxyAddr,
    target: &HttpsUrl,
    timeout: Duration,
) -> Result<TcpStream, DownloadError> {
    let mut stream = tcp_connect(&proxy.host, proxy.port, timeout)?;
    let request = format!(
        "CONNECT {0}:{1} HTTP/1.1\r\nHost: {0}:{1}\r\n\r\n",
        target.host, target.port
    );
    stream.write_all(request.as_bytes()).map_err(io_error)?;
    stream.flush().map_err(io_error)?;

    // 逐字节读代理响应头，**绝不越过 "\r\n\r\n"**：隧道内的 TLS 字节可能已经
    // 到达，用 BufReader 预读再 into_inner 会把它们吞掉，握手就死了。
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        stream.read_exact(&mut byte).map_err(io_error)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
        if head.len() > MAX_HEADER_LINE {
            return Err(DownloadError::Proxy("代理响应头超长".into()));
        }
    }
    let status_line = String::from_utf8_lossy(&head);
    if !status_line.lines().next().unwrap_or("").contains(" 200") {
        return Err(DownloadError::Proxy(format!(
            "CONNECT 被拒绝：{}",
            status_line.lines().next().unwrap_or("").trim()
        )));
    }
    Ok(stream)
}

fn open_tls(
    config: &DownloadConfig,
    url: &HttpsUrl,
) -> Result<BufReader<TlsStream>, DownloadError> {
    let name = server_name(&url.host)?;
    let tls = rustls::ClientConnection::new(std::sync::Arc::new(client_config(config)?), name)
        .map_err(|e| DownloadError::Tls(e.to_string()))?;

    let raw = match &config.proxy {
        Some(proxy) => connect_via_proxy(proxy, url, config.connect_timeout)?,
        None => tcp_connect(&url.host, url.port, config.connect_timeout)?,
    };
    let mut stream = rustls::StreamOwned::new(tls, raw);
    // 把 TLS 握手彻底走完并送出排队数据：否则请求会停在 rustls 的发送队列里，
    // read 干等响应直到超时——握手期间的排队明文只有在握手完成后才可能发出。
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| DownloadError::Tls(e.to_string()))?;
    }
    stream.flush().map_err(io_error)?;
    Ok(BufReader::with_capacity(CHUNK, stream))
}

fn read_limited_line<R: BufRead>(
    reader: &mut R,
    line: &mut String,
) -> Result<usize, DownloadError> {
    read_line_or_eof(reader, line)
}

struct HeadData {
    status: u16,
    headers: Vec<(String, String)>,
}

fn read_head(reader: &mut BufReader<TlsStream>) -> Result<HeadData, DownloadError> {
    let mut status = None;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = read_limited_line(reader, &mut line)?;
        if read == 0 {
            return Err(DownloadError::Protocol("响应头在结束前被对端截断".into()));
        }
        if status.is_none() {
            let code = line
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| DownloadError::Protocol(format!("状态行不合法：{}", line.trim())))?;
            status = Some(code);
            continue;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
        if headers.len() > 128 {
            return Err(DownloadError::Protocol("响应头数量超限".into()));
        }
    }
    let status = status.ok_or_else(|| DownloadError::Protocol("缺少状态行".into()))?;
    Ok(HeadData { status, headers })
}

/// 发出 GET 请求并读取响应头。
fn request_and_read_head(
    config: &DownloadConfig,
    url: &HttpsUrl,
    extra_headers: &[(&str, &str)],
) -> Result<(ResponseHead, BufReader<TlsStream>), DownloadError> {
    let mut reader = open_tls(config, url)?;
    let authority = if url.port == HTTPS_DEFAULT_PORT {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: taoism-agent/fulu\r\nAccept: application/octet-stream\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        path = url.path
    );
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    reader
        .get_mut()
        .write_all(request.as_bytes())
        .map_err(io_error)?;
    reader.get_mut().flush().map_err(io_error)?;

    let data = read_head(&mut reader)?;
    let content_length = data
        .headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse::<u64>().ok());
    Ok((
        ResponseHead {
            status: data.status,
            content_length,
            headers: data.headers,
            final_url: url.to_string_lossy(),
            redirect_hops: Vec::new(),
        },
        reader,
    ))
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// 解析 Location：只接受同主机相对路径或 `https://` 绝对地址。
fn resolve_location(current: &HttpsUrl, location: &str) -> Result<HttpsUrl, DownloadError> {
    let location = location.trim();
    if location.len() > MAX_LOCATION {
        return Err(DownloadError::Protocol("Location 头超长".into()));
    }
    if let Some(rest) = location.strip_prefix("http://") {
        let _ = rest;
        return Err(DownloadError::RedirectInsecure(format!(
            "重定向把流量降级到明文 HTTP：{location}；拒绝跟随"
        )));
    }
    if location.starts_with("https://") {
        return HttpsUrl::parse(location);
    }
    if location.starts_with('/') {
        return Ok(current.with_path(location));
    }
    Err(DownloadError::Protocol(format!(
        "不支持的重定向 Location 形式：{location}"
    )))
}

fn feed(
    chunk: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> Result<(), DownloadError>,
    hasher: &mut Sha256,
    received: &mut u64,
    max_body: Option<u64>,
) -> Result<(), DownloadError> {
    if chunk.is_empty() {
        return Ok(());
    }
    *received += chunk.len() as u64;
    if let Some(limit) = max_body
        && *received > limit
    {
        return Err(DownloadError::BodyTooLarge {
            limit,
            received: *received,
        });
    }
    hasher.update(chunk);
    sink(chunk)
}

fn stream_body(
    reader: &mut BufReader<TlsStream>,
    head: &ResponseHead,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), DownloadError>,
    on_progress: &mut dyn FnMut(u64, Option<u64>) -> Result<(), DownloadError>,
    max_body: Option<u64>,
) -> Result<(u64, String), DownloadError> {
    let chunked = head
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buffer = vec![0_u8; CHUNK];

    if chunked {
        loop {
            let mut size_line = String::new();
            if read_limited_line(reader, &mut size_line)? == 0 {
                return Err(DownloadError::Protocol("chunked 响应在结束前被截断".into()));
            }
            let size_text = size_line.trim().split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16)
                .map_err(|_| DownloadError::Protocol(format!("chunk 长度不合法：{size_text}")))?;
            if size == 0 {
                // 结束分块；吃掉 trailer 直到空行。
                loop {
                    let mut trailer = String::new();
                    let read = read_limited_line(reader, &mut trailer)?;
                    if read == 0 || trailer == "\r\n" || trailer == "\n" {
                        break;
                    }
                }
                break;
            }
            let mut remaining = size;
            while remaining > 0 {
                let want = remaining.min(CHUNK);
                let read = read_chunk_or_eof(reader, &mut buffer[..want])?;
                if read == 0 {
                    return Err(DownloadError::Protocol(
                        "chunked 响应在结束标记之前被截断".into(),
                    ));
                }
                feed(&buffer[..read], sink, &mut hasher, &mut received, max_body)?;
                on_progress(received, head.content_length)?;
                remaining -= read;
            }
            let mut crlf = [0_u8; 2];
            reader.read_exact(&mut crlf).map_err(io_error)?;
        }
    } else if let Some(length) = head.content_length {
        let mut remaining = length;
        while remaining > 0 {
            let want = remaining.min(CHUNK as u64) as usize;
            let read = read_chunk_or_eof(reader, &mut buffer[..want])?;
            if read == 0 {
                return Err(DownloadError::Protocol(format!(
                    "响应体被截断：声明 {length} 字节，实收 {} 字节",
                    received
                )));
            }
            feed(&buffer[..read], sink, &mut hasher, &mut received, max_body)?;
            on_progress(received, Some(length))?;
            remaining -= read as u64;
        }
    } else {
        // 无 Content-Length：读到连接关闭（Connection: close 下的传统语义）。
        loop {
            let read = read_chunk_or_eof(reader, &mut buffer)?;
            if read == 0 {
                break;
            }
            feed(&buffer[..read], sink, &mut hasher, &mut received, max_body)?;
            on_progress(received, head.content_length)?;
        }
    }

    let digest = hasher.finalize();
    Ok((
        received,
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    ))
}

/// 带 5 跳重定向的完整 GET：响应体流式交给 `sink`，返回实测字节数与 sha256。
#[allow(clippy::too_many_arguments)]
pub fn fetch(
    config: &DownloadConfig,
    url: &str,
    extra_headers: &[(&str, &str)],
    sink: &mut dyn FnMut(&[u8]) -> Result<(), DownloadError>,
    on_response: &mut dyn FnMut(&ResponseHead) -> Result<(), DownloadError>,
    on_progress: &mut dyn FnMut(u64, Option<u64>) -> Result<(), DownloadError>,
) -> Result<Downloaded, DownloadError> {
    let mut current = HttpsUrl::parse(url)?;
    let mut hops: Vec<String> = Vec::new();

    for _hop in 0..=config.max_redirects {
        let (mut head, mut reader) = request_and_read_head(config, &current, extra_headers)?;

        if is_redirect(head.status) {
            let location = head
                .header("location")
                .ok_or_else(|| {
                    DownloadError::Protocol(format!("HTTP {} 缺少 Location 头", head.status))
                })?
                .to_owned();
            let next = resolve_location(&current, &location)?;
            hops.push(next.to_string_lossy());
            current = next;
            continue;
        }

        head.redirect_hops = hops.clone();
        on_response(&head)?;
        let (bytes, sha256) = stream_body(&mut reader, &head, sink, on_progress, config.max_body)?;
        return Ok(Downloaded {
            status: head.status,
            final_url: head.final_url,
            bytes,
            sha256,
            redirect_hops: hops,
        });
    }

    Err(DownloadError::RedirectTooMany(format!(
        "超过 {} 跳重定向仍未拿到最终响应",
        config.max_redirects
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_parse_requires_http_prefix_and_port() {
        let proxy = ProxyAddr::parse("http://127.0.0.1:7890").unwrap();
        assert_eq!(proxy.host, "127.0.0.1");
        assert_eq!(proxy.port, 7890);
        assert!(matches!(
            ProxyAddr::parse("https://127.0.0.1:7890"),
            Err(DownloadError::Proxy(_))
        ));
        assert!(matches!(
            ProxyAddr::parse("127.0.0.1:7890"),
            Err(DownloadError::Proxy(_))
        ));
        assert!(matches!(
            ProxyAddr::parse("http://127.0.0.1"),
            Err(DownloadError::Proxy(_))
        ));
        assert!(matches!(
            ProxyAddr::parse("http://127.0.0.1:0"),
            Err(DownloadError::Proxy(_))
        ));
    }

    #[test]
    fn redirect_only_accepts_https_or_same_host_paths() {
        let current = HttpsUrl::parse("https://huggingface.co/a/b").unwrap();
        assert_eq!(
            resolve_location(&current, "/cdn/file.gguf")
                .unwrap()
                .to_string_lossy(),
            "https://huggingface.co/cdn/file.gguf"
        );
        assert_eq!(
            resolve_location(&current, "https://cdn-lfs.example.com/x")
                .unwrap()
                .to_string_lossy(),
            "https://cdn-lfs.example.com/x"
        );
        assert!(matches!(
            resolve_location(&current, "http://cdn.example.com/x"),
            Err(DownloadError::RedirectInsecure(_))
        ));
        assert!(matches!(
            resolve_location(&current, "ftp://cdn.example.com/x"),
            Err(DownloadError::Protocol(_))
        ));
    }

    #[test]
    fn error_display_is_actionable() {
        let error = DownloadError::RedirectInsecure("demo".into());
        assert!(error.to_string().contains("明文"));
        let error = DownloadError::HttpStatus(429, "rate limited".into());
        assert!(error.to_string().contains("429"));
    }
}

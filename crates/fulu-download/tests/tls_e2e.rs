//! 真实 TLS 集成测试：在本机回环上起 rustls TLS 服务（自签证书，走
//! `trust_root_der` 测试钩子），验证握手、流式响应体、chunked、重定向、
//! 代理 CONNECT 隧道与断点续传语义。全部走真实 TCP，不做 mock 假流。

use fulu_download::{DownloadConfig, DownloadError, ProxyAddr, fetch};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 自签证书（DNS localhost + IP 127.0.0.1 双 SAN），返回 (cert_der, key_pkcs8_der)。
fn self_signed() -> (Vec<u8>, Vec<u8>) {
    let signed =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("自签证书");
    (signed.cert.der().to_vec(), signed.key_pair.serialize_der())
}

/// 在回环上起真实 rustls TLS 服务；每个连接读出请求路径后按回调写响应，写完即关。
struct TestServer {
    port: u16,
    root_der: Vec<u8>,
}

fn spawn_server(response_for: impl Fn(&str) -> Vec<u8> + Send + 'static) -> TestServer {
    let (cert_der, key_der) = self_signed();
    let root_for_client = cert_der.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("监听回环");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("协议版本")
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls_pki_types::CertificateDer::from(cert_der)],
                    rustls_pki_types::PrivateKeyDer::Pkcs8(
                        rustls_pki_types::PrivatePkcs8KeyDer::from(key_der),
                    ),
                )
                .expect("服务端证书"),
        );
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let conn = match rustls::ServerConnection::new(config.clone()) {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            let mut tls = rustls::StreamOwned::new(conn, stream);
            let path = read_request_path(&mut tls);
            let response = response_for(&path);
            let _ = tls.write_all(&response);
            let _ = tls.flush();
            // 与真实服务器一致：写完发 close_notify 再关，客户端才能拿到干净的 EOF。
            tls.conn.send_close_notify();
            let _ = tls.flush();
            // drop 即关。
        }
    });
    TestServer {
        port,
        root_der: root_for_client,
    }
}

fn read_request_path(tls: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>) -> String {
    let mut reader = BufReader::new(tls);
    let mut line = String::new();
    // 读状态行；TLS 握手完成后才读得到。
    for _ in 0..20 {
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => break,
            _ => line.clear(),
        }
    }
    let mut header_end = String::new();
    loop {
        header_end.clear();
        if reader.read_line(&mut header_end).unwrap_or(1) == 0
            || header_end == "\r\n"
            || header_end == "\n"
        {
            break;
        }
    }
    line.split_whitespace().nth(1).unwrap_or("/").to_owned()
}

fn response(status_line: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("{status_line}\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn noop_response(_head: &fulu_download::ResponseHead) -> Result<(), DownloadError> {
    Ok(())
}

fn noop_progress(_received: u64, _total: Option<u64>) -> Result<(), DownloadError> {
    Ok(())
}

fn config_for(server: &TestServer) -> DownloadConfig {
    DownloadConfig {
        trust_root_der: Some(server.root_der.clone()),
        ..DownloadConfig::default()
    }
}

#[test]
fn get_with_content_length_streams_and_hashes() {
    let body = b"hello fulu weights";
    let server = spawn_server(move |path| {
        if path == "/model.gguf" {
            response(
                "HTTP/1.1 200 OK",
                &[("Content-Length", &body.len().to_string())],
                body,
            )
        } else {
            response(
                "HTTP/1.1 404 Not Found",
                &[("Content-Length", "5")],
                b"nope!",
            )
        }
    });
    let config = DownloadConfig {
        trust_root_der: Some(server.root_der.clone()),
        ..DownloadConfig::default()
    };
    let mut received = Vec::new();
    let downloaded = fetch(
        &config,
        &format!("https://127.0.0.1:{}/model.gguf", server.port),
        &[],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut noop_response,
        &mut noop_progress,
    )
    .expect("下载成功");
    assert_eq!(downloaded.status, 200);
    assert_eq!(received, body);
    assert_eq!(downloaded.bytes, body.len() as u64);
    assert_eq!(downloaded.sha256, sha256_hex(body));
    assert!(downloaded.redirect_hops.is_empty());
}

#[test]
fn chunked_response_is_reambled_correctly() {
    let server = spawn_server(|_path| {
        let mut out =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk in [&b"alpha"[..], &b"-beta"[..], &b"-gamma"[..]] {
            out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    });
    let mut received = Vec::new();
    let downloaded = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/chunked", server.port),
        &[],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut noop_response,
        &mut noop_progress,
    )
    .expect("chunked 下载成功");
    assert_eq!(received, b"alpha-beta-gamma");
    assert_eq!(downloaded.sha256, sha256_hex(b"alpha-beta-gamma"));
}

#[test]
fn https_redirect_is_followed_and_recorded() {
    let final_body = b"redirected payload";
    let port_holder: std::sync::Mutex<Option<u16>> = std::sync::Mutex::new(None);
    let holder = Arc::new(port_holder);
    let holder_clone = holder.clone();
    let server = spawn_server(move |path| {
        let port = holder_clone.lock().unwrap().unwrap();
        if path == "/start" {
            response(
                "HTTP/1.1 302 Found",
                &[("Location", &format!("https://127.0.0.1:{port}/final"))],
                b"",
            )
        } else {
            response(
                "HTTP/1.1 200 OK",
                &[("Content-Length", &final_body.len().to_string())],
                final_body,
            )
        }
    });
    *holder.lock().unwrap() = Some(server.port);

    let mut received = Vec::new();
    let downloaded = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/start", server.port),
        &[],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut noop_response,
        &mut noop_progress,
    )
    .expect("重定向应被跟随");
    assert_eq!(received, final_body);
    assert_eq!(downloaded.redirect_hops.len(), 1);
    assert!(downloaded.redirect_hops[0].ends_with("/final"));
    assert!(downloaded.final_url.ends_with("/final"));
}

#[test]
fn redirect_to_plain_http_is_rejected() {
    let server = spawn_server(|_path| {
        response(
            "HTTP/1.1 302 Found",
            &[("Location", "http://127.0.0.1/x")],
            b"",
        )
    });
    let error = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/down", server.port),
        &[],
        &mut |_| Ok(()),
        &mut noop_response,
        &mut noop_progress,
    )
    .expect_err("明文重定向必须拒绝");
    assert!(
        matches!(error, DownloadError::RedirectInsecure(_)),
        "{error}"
    );
}

#[test]
fn redirect_loop_is_capped() {
    let server =
        spawn_server(|_path| response("HTTP/1.1 302 Found", &[("Location", "/next")], b""));
    let error = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/loop", server.port),
        &[],
        &mut |_| Ok(()),
        &mut noop_response,
        &mut noop_progress,
    )
    .expect_err("重定向环必须有上限");
    assert!(
        matches!(error, DownloadError::RedirectTooMany(_)),
        "{error}"
    );
}

#[test]
fn error_status_carries_code_and_body() {
    let server = spawn_server(|_path| {
        response(
            "HTTP/1.1 429 Too Many Requests",
            &[("Content-Length", "12")],
            b"rate limited",
        )
    });
    let mut received = Vec::new();
    let downloaded = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/busy", server.port),
        &[],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut noop_response,
        &mut noop_progress,
    )
    .expect("库层返回状态码，错误判定是调用方策略");
    assert_eq!(downloaded.status, 429);
    assert_eq!(received, b"rate limited");
}

#[test]
fn max_body_cap_is_enforced() {
    let body = vec![7_u8; 1024];
    let server = spawn_server(move |_path| {
        response(
            "HTTP/1.1 200 OK",
            &[("Content-Length", &body.len().to_string())],
            &body,
        )
    });
    let config = DownloadConfig {
        trust_root_der: Some(server.root_der.clone()),
        max_body: Some(100),
        ..DownloadConfig::default()
    };
    let error = fetch(
        &config,
        &format!("https://127.0.0.1:{}/big", server.port),
        &[],
        &mut |_| Ok(()),
        &mut noop_response,
        &mut noop_progress,
    )
    .expect_err("超上限必须失败");
    assert!(
        matches!(error, DownloadError::BodyTooLarge { .. }),
        "{error}"
    );
}

#[test]
fn range_request_reaches_server_and_206_is_delivered() {
    let server = spawn_server(|path| {
        if path == "/part" {
            // 真实断点续传语义：206 Partial Content。
            response(
                "HTTP/1.1 206 Partial Content",
                &[("Content-Length", "5")],
                b"world",
            )
        } else {
            response("HTTP/1.1 404 Not Found", &[("Content-Length", "1")], b"x")
        }
    });
    let seen_status = std::cell::RefCell::new(None);
    let mut received = Vec::new();
    let downloaded = fetch(
        &config_for(&server),
        &format!("https://127.0.0.1:{}/part", server.port),
        &[("Range", "bytes=6-")],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut |head| {
            *seen_status.borrow_mut() = Some(head.status);
            Ok(())
        },
        &mut noop_progress,
    )
    .expect("断点续传下载成功");
    assert_eq!(seen_status.into_inner(), Some(206));
    assert_eq!(received, b"world");
    assert_eq!(downloaded.status, 206);
}

#[test]
fn connect_proxy_tunnels_tls_end_to_end() {
    let body = b"through the tunnel";
    let inner = spawn_server(move |path| {
        if path == "/tunnel" {
            response(
                "HTTP/1.1 200 OK",
                &[("Content-Length", &body.len().to_string())],
                body,
            )
        } else {
            response("HTTP/1.1 404 Not Found", &[("Content-Length", "1")], b"x")
        }
    });

    // mock 代理：只做 CONNECT 握手 + 原始字节双向搬运。
    let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("代理监听");
    let proxy_port = proxy_listener.local_addr().unwrap().port();
    let target_port = inner.port;
    std::thread::spawn(move || {
        for stream in proxy_listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut first_line = String::new();
            loop {
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 || request == "\r\n" {
                    break;
                }
                if first_line.is_empty() {
                    first_line = request.clone();
                }
            }
            if !first_line.starts_with(&format!("CONNECT 127.0.0.1:{target_port}")) {
                let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
                continue;
            }
            let _ = stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
            let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", target_port)) else {
                continue;
            };
            let mut downstream = stream.try_clone().expect("clone");
            let mut up_read = upstream.try_clone().expect("clone");
            // 上游 → 客户端方向（服务端响应回传）；主线程负责客户端 → 上游。
            let up_to_down = std::thread::spawn(move || {
                let _ = std::io::copy(&mut up_read, &mut downstream);
            });
            let _ = std::io::copy(&mut reader, &mut upstream);
            let _ = up_to_down.join();
        }
    });

    let config = DownloadConfig {
        proxy: Some(ProxyAddr::parse(&format!("http://127.0.0.1:{proxy_port}")).unwrap()),
        trust_root_der: Some(inner.root_der.clone()),
        ..DownloadConfig::default()
    };
    let mut received = Vec::new();
    let downloaded = fetch(
        &config,
        &format!("https://127.0.0.1:{}/tunnel", inner.port),
        &[],
        &mut |chunk| {
            received.extend_from_slice(chunk);
            Ok(())
        },
        &mut noop_response,
        &mut noop_progress,
    )
    .expect("代理隧道下载成功");
    assert_eq!(received, body);
    assert_eq!(downloaded.sha256, sha256_hex(body));
}

#[test]
fn plain_tcp_target_is_rejected_by_tls_handshake() {
    // 目标端口没有 TLS 服务（明文），TLS 握手必须失败而不是拿到乱码。
    let plain = TcpListener::bind("127.0.0.1:0").expect("明文监听");
    let port = plain.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in plain.incoming() {
            let mut stream = stream.unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\nplain");
        }
    });
    let config = DownloadConfig::default();
    let error = fetch(
        &config,
        &format!("https://127.0.0.1:{port}/plain"),
        &[],
        &mut |_| Ok(()),
        &mut noop_response,
        &mut noop_progress,
    )
    .expect_err("明文目标必须握手失败");
    assert!(
        matches!(
            error,
            DownloadError::Tls(_) | DownloadError::Io(_) | DownloadError::Protocol(_)
        ),
        "{error}"
    );
}

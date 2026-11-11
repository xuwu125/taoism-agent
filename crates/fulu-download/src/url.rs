//! 极小 HTTPS URL 解析：只接受 `https://`，路径与查询串**原样保留**（不编码、不重排）。
//!
//! 明确拒绝的输入（各自给出可读原因，不猜意图）：
//! - 非 https 方案（模型下载没有「先试 http 再升级」的余地）；
//! - userinfo（`user:pass@host`——凭据不该出现在 URL 里）；
//! - IPv6 字面量（方括号语法；HF 下载用不到，明确拒绝比半吊子支持诚实）；
//! - 空主机名、非法端口。

use crate::DownloadError;

/// 默认 HTTPS 端口。
pub const HTTPS_DEFAULT_PORT: u16 = 443;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsUrl {
    pub host: String,
    pub port: u16,
    /// 以 `/` 开头，含查询串，原样保留。
    pub path: String,
}

impl HttpsUrl {
    pub fn parse(raw: &str) -> Result<Self, DownloadError> {
        let rest = raw.strip_prefix("https://").ok_or_else(|| {
            DownloadError::InvalidUrl(format!("只接受 https:// 地址，实际输入：{raw}"))
        })?;

        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(DownloadError::InvalidUrl("缺少主机名".into()));
        }
        if authority.contains('@') {
            return Err(DownloadError::InvalidUrl(
                "URL 不允许携带 userinfo（user:pass@host）；凭据请走环境变量或代理配置".into(),
            ));
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port_text)) => {
                if host.is_empty() {
                    return Err(DownloadError::InvalidUrl("主机名为空".into()));
                }
                if host.starts_with('[') {
                    return Err(DownloadError::InvalidUrl(
                        "不支持 IPv6 字面量主机（方括号语法）".into(),
                    ));
                }
                let port: u16 = port_text
                    .parse()
                    .map_err(|_| DownloadError::InvalidUrl(format!("端口不合法：{port_text}")))?;
                if port == 0 {
                    return Err(DownloadError::InvalidUrl("端口不能为 0".into()));
                }
                (host, port)
            }
            None => {
                if authority.starts_with('[') {
                    return Err(DownloadError::InvalidUrl(
                        "不支持 IPv6 字面量主机（方括号语法）".into(),
                    ));
                }
                (authority, HTTPS_DEFAULT_PORT)
            }
        };

        let host = host.to_ascii_lowercase();
        if host.parse::<std::net::Ipv4Addr>().is_err() && !is_domain_name(&host) {
            return Err(DownloadError::InvalidUrl(format!("主机名不合法：{host}")));
        }

        Ok(Self {
            host,
            port,
            path: path.to_owned(),
        })
    }

    /// 重定向落在同一主机（Location 只有路径）时重建 URL。
    pub fn with_path(&self, path: &str) -> Self {
        let mut next = self.clone();
        next.path = path.to_owned();
        next
    }

    pub fn to_string_lossy(&self) -> String {
        let authority = if self.port == HTTPS_DEFAULT_PORT {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        format!("https://{authority}{}", self.path)
    }
}

fn is_domain_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_path_and_query_verbatim() {
        let url = HttpsUrl::parse(
            "https://huggingface.co/LiquidAI/LFM3-1.7B-GGUF/resolve/main/LFM3-1.7B-Q4_K_M.gguf",
        )
        .unwrap();
        assert_eq!(url.host, "huggingface.co");
        assert_eq!(url.port, HTTPS_DEFAULT_PORT);
        assert_eq!(
            url.path,
            "/LiquidAI/LFM3-1.7B-GGUF/resolve/main/LFM3-1.7B-Q4_K_M.gguf"
        );
    }

    #[test]
    fn default_path_is_root_slash() {
        let url = HttpsUrl::parse("https://example.com").unwrap();
        assert_eq!(url.path, "/");
        let query = HttpsUrl::parse("https://example.com/api/tree?recursive=true").unwrap();
        assert_eq!(query.path, "/api/tree?recursive=true");
    }

    #[test]
    fn explicit_port_is_kept() {
        let url = HttpsUrl::parse("https://example.com:8443/a").unwrap();
        assert_eq!(url.port, 8443);
    }

    #[test]
    fn plain_http_is_rejected() {
        assert!(matches!(
            HttpsUrl::parse("http://example.com/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
        assert!(matches!(
            HttpsUrl::parse("example.com/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
    }

    #[test]
    fn userinfo_is_rejected() {
        assert!(matches!(
            HttpsUrl::parse("https://user:pass@example.com/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
    }

    #[test]
    fn ipv6_literal_is_rejected() {
        assert!(matches!(
            HttpsUrl::parse("https://[::1]:443/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
    }

    #[test]
    fn bad_host_and_port_are_rejected() {
        assert!(matches!(
            HttpsUrl::parse("https:///a"),
            Err(DownloadError::InvalidUrl(_))
        ));
        assert!(matches!(
            HttpsUrl::parse("https://example.com:0/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
        assert!(matches!(
            HttpsUrl::parse("https://example.com:notaport/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
        assert!(matches!(
            HttpsUrl::parse("https://bad host/a"),
            Err(DownloadError::InvalidUrl(_))
        ));
    }

    #[test]
    fn with_path_rebuilds_on_same_host() {
        let url = HttpsUrl::parse("https://example.com/one?a=1").unwrap();
        let next = url.with_path("/two?b=2");
        assert_eq!(next.to_string_lossy(), "https://example.com/two?b=2");
    }

    #[test]
    fn host_is_lowercased() {
        let url = HttpsUrl::parse("https://HuGgInGfAcE.CoM/x").unwrap();
        assert_eq!(url.host, "huggingface.com");
    }
}

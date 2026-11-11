//! 最小 HTTPS/1.1 GET 客户端，**只服务 `model seal` / `model fetch` 的显式下载链路**。
//!
//! 设计取舍（docs/02 第 8.5 节、第 39 节）：
//! - 这是依赖白名单里 **TLS 的唯一例外落点**：rustls + webpki-roots，其余任何 crate
//!   仍不得引入 TLS；provider / moli / MCP 的回环链路保持明文，职责边界不混。
//! - 不引入 reqwest / hyper / tokio：GET、重定向、chunked、断点续传用 `std::net`
//!   + rustls 手写，依赖面与内存面都最小。
//! - 信任锚固定为内嵌的 Mozilla 根证书集（webpki-roots），**不读系统证书库**，
//!   校验行为不随系统状态漂移。
//! - 重定向最多 5 跳，且每一跳都必须仍是 `https://`——降级到明文一律拒绝。
//! - 响应体**流式**交给调用方落盘并同步计算 sha256，1.1 GB 权重不进内存。

pub mod client;
pub mod url;

pub use client::{DownloadConfig, DownloadError, Downloaded, ProxyAddr, ResponseHead, fetch};
pub use url::HttpsUrl;

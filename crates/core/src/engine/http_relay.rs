//! HTTP 转发公共层：正向代理（绝对形式请求）与反向代理共用。
//!
//! 职责有三件：
//!
//! 1. 头部清洗——剥掉 hop-by-hop 头（RFC 9110 §7.6.1），重写 `Host`。
//! 2. 上游连接——按规则拨号，必要时叠加 TLS，然后交给 hyper 的 http1 客户端。
//! 3. 响应中继——请求体与响应体全程流式，不落地缓冲，因此 SSE 能逐块送达。
//!
//! 这里不碰 `Authorization`、`x-api-key`、`anthropic-*`、`openai-*` 等头，
//! 它们原样透传，也从不写进日志。

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::TokioIo;

use crate::config::Upstream;
use crate::engine::upstream;

/// 处理器统一的错误类型。连接级错误都被包成它，由调用方决定记日志还是回 502。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 响应体：上游响应直接装箱透传，自造的错误页用 [`text_response`] 生成。
pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// Hop-by-hop 头（RFC 9110 §7.6.1）——只对单跳有意义，不能转发。
///
/// `proxy-authorization` 也在内：它是客户端给本代理的凭据，绝不能泄露给上游。
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// 构造纯文本响应（错误页 / 认证质询）。
pub fn text_response(status: StatusCode, body: &'static str) -> Response<ProxyBody> {
    let mut resp = Response::new(
        Full::new(Bytes::from_static(body.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = status;
    resp
}

/// 空响应体，用于 CONNECT 的 200 应答。
pub fn empty_body() -> ProxyBody {
    Full::new(Bytes::new()).map_err(|never| match never {}).boxed()
}

/// 剥掉 hop-by-hop 头，以及 `Connection:` 里点名的扩展头。
///
/// 用 `append` 而非 `insert`，多值头（`Set-Cookie`、`Via` 等）不会被压成一条。
pub fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    let mut listed: Vec<String> = Vec::new();
    for value in headers.get_all(http::header::CONNECTION) {
        if let Ok(text) = value.to_str() {
            for item in text.split(',') {
                let name = item.trim().to_ascii_lowercase();
                if !name.is_empty() {
                    listed.push(name);
                }
            }
        }
    }

    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str()) || listed.contains(&lower) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// 生成 `Host` 头的值：端口是该 scheme 的默认端口时省略，IPv6 字面量补方括号。
pub fn host_header(host: &str, port: u16, default_port: u16) -> String {
    let bracketed = host.contains(':') && !host.starts_with('[');
    let host = if bracketed {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    if port == default_port {
        host
    } else {
        format!("{host}:{port}")
    }
}

/// 覆写 `Host` 头。取值非法（含控制字符等）时保持原样，不让一个坏头把请求整条毁掉。
pub fn set_host(headers: &mut HeaderMap, host: &str) {
    if let Ok(value) = HeaderValue::from_str(host) {
        headers.insert(http::header::HOST, value);
    }
}

/// 把请求发往上游并返回上游响应。
///
/// `req` 必须已经是可直接上线的形态：origin-form URI、清洗过的头、正确的 `Host`。
/// `use_tls` 为真时在拨通的连接上再握一次手，SNI 用 `host`。
///
/// 连接驱动任务随响应体存活；响应体被读完或丢弃后它自然结束。
pub async fn send_upstream(
    upstream_cfg: &Upstream,
    use_tls: bool,
    host: &str,
    port: u16,
    req: Request<Incoming>,
) -> Result<Response<Incoming>, BoxError> {
    let conn = upstream::dial(upstream_cfg, host, port).await?;
    let conn = if use_tls {
        upstream::wrap_tls(conn, host).await?
    } else {
        conn
    };

    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(conn)).await?;

    tokio::spawn(async move {
        // 上游主动断开是常态（`Connection: close`、SSE 结束），故只记 debug。
        if let Err(e) = connection.await {
            tracing::debug!("upstream connection ended: {e}");
        }
    });

    Ok(sender.send_request(req).await?)
}

/// 把上游响应转成下游响应：状态码照抄，头部剥 hop-by-hop，响应体装箱流式透传。
pub fn relay_response(upstream_resp: Response<Incoming>) -> Response<ProxyBody> {
    let (parts, body) = upstream_resp.into_parts();
    let mut resp = Response::new(body.boxed());
    *resp.status_mut() = parts.status;
    *resp.version_mut() = http::Version::HTTP_11;
    *resp.headers_mut() = strip_hop_by_hop(&parts.headers);
    resp
}

/// 响应是否为 SSE 流，仅用于日志标注。
pub fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

/// 去掉查询串，只留路径。
///
/// 日志一律只记路径：查询串里可能带着 API key（`?key=…`），写进日志就等于泄密。
pub fn path_only(path_and_query: &str) -> &str {
    match path_and_query.split_once('?') {
        Some((path, _)) => path,
        None => path_and_query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_and_connection_listed_headers_are_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("close, x-custom"));
        headers.insert("proxy-authorization", HeaderValue::from_static("Basic xx"));
        headers.insert("x-custom", HeaderValue::from_static("1"));
        headers.insert("authorization", HeaderValue::from_static("Bearer token"));
        headers.append("via", HeaderValue::from_static("1.1 a"));
        headers.append("via", HeaderValue::from_static("1.1 b"));

        let out = strip_hop_by_hop(&headers);
        assert!(out.get("connection").is_none());
        assert!(out.get("proxy-authorization").is_none());
        assert!(out.get("x-custom").is_none());
        // 业务头原样保留
        assert_eq!(out.get("authorization").unwrap(), "Bearer token");
        assert_eq!(out.get_all("via").iter().count(), 2);
    }

    #[test]
    fn host_header_omits_default_port() {
        assert_eq!(host_header("api.example.com", 443, 443), "api.example.com");
        assert_eq!(
            host_header("api.example.com", 8443, 443),
            "api.example.com:8443"
        );
        assert_eq!(host_header("::1", 8080, 80), "[::1]:8080");
    }

    #[test]
    fn path_only_drops_query() {
        assert_eq!(path_only("/v1/messages?key=secret"), "/v1/messages");
        assert_eq!(path_only("/v1/messages"), "/v1/messages");
    }
}

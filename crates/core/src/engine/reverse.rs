//! 反向代理处理器。
//!
//! 接受入站请求，全部转发到 `rule.target`，并处理：
//!
//! - 路径令牌认证（剥掉令牌再转发）
//! - Basic 认证（走 `Proxy-Authorization`，见下）
//! - `Host` 重写与目标基路径拼接
//! - Hop-by-hop 头剥离（RFC 9110 §7.6.1）
//! - `https://` 目标的 TLS 握手
//! - 请求体与响应体全程流式，SSE 逐块送达，不做整体缓冲
//! - `Authorization` / `x-api-key` / `anthropic-*` / `openai-*` 原样透传
//!
//! 为什么 Basic 认证读 `Proxy-Authorization` 而不是 `Authorization`：后者装的是
//! 客户端发给 AI 服务的 API key，必须原样透传。若本代理去消费它，密钥就到不了
//! 目标服务；回 401 又会诱导客户端把凭据塞进同一个头。因此代理自己的凭据单独走
//! `Proxy-Authorization`，失败回 407，该头随即被当作 hop-by-hop 剥掉。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use http::{Request, Response, StatusCode, Uri};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::Rule;
use crate::engine::auth;
use crate::engine::forward::format_duration;
use crate::engine::http_relay::{
    self, relay_response, send_upstream, text_response, BoxError, ProxyBody,
};
use crate::engine::listener::HANDSHAKE_TIMEOUT;
use crate::status::Stats;

// ─────────────── 公开入口 ───────────────

/// 处理一条反向代理连接。`stream` 已是明文（TLS 入站在上层解密）。
pub async fn handle_reverse<S>(
    stream: S,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> Result<(), BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let rule = Arc::clone(rule);
    let stats = Arc::clone(stats);

    // 不设 timer 的话 hyper 默认的请求头超时不生效，慢吞吞发头的连接会一直挂着。
    hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HANDSHAKE_TIMEOUT)
        .serve_connection(
            TokioIo::new(stream),
            service_fn(move |req: Request<Incoming>| {
                let rule = Arc::clone(&rule);
                let stats = Arc::clone(&stats);
                async move { serve_reverse(req, peer_addr, rule, stats).await }
            }),
        )
        .await?;
    Ok(())
}

// ─────────────── 核心处理 ───────────────

async fn serve_reverse(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
) -> Result<Response<ProxyBody>, std::convert::Infallible> {
    // ── Basic 认证（未配置时 check_basic 直接放行）──
    let credential = req
        .headers()
        .get(http::header::PROXY_AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !auth::check_basic(&rule.auth, credential).is_ok() {
        stats.auth_fail();
        log_auth_failure(&rule, peer_addr);
        let mut resp = text_response(
            StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            "Proxy Authentication Required",
        );
        resp.headers_mut().insert(
            http::header::PROXY_AUTHENTICATE,
            http::HeaderValue::from_static("Basic realm=\"AOProxy\""),
        );
        return Ok(resp);
    }

    // ── 路径令牌认证（未配置时原样返回）──
    let original_path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());

    let Ok(stripped_path) = auth::check_path_token(&rule.auth, &original_path) else {
        stats.auth_fail();
        log_auth_failure(&rule, peer_addr);
        // 回 404 而不是 401：令牌错误与路径不存在对外表现一致，不给探测者信号。
        return Ok(text_response(StatusCode::NOT_FOUND, "Not Found"));
    };

    // ── 解析目标 ──
    let Some(target) = rule.target.as_deref().and_then(parse_target) else {
        stats.error();
        tracing::warn!(rule_id = %rule.id, "rule has no usable target");
        return Ok(text_response(StatusCode::BAD_GATEWAY, "Bad Gateway"));
    };

    let forward_path = join_path(&target.base_path, &stripped_path);
    let method = req.method().clone();
    tracing::debug!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.request",
            &[
                ("method", method.as_str()),
                ("path", http_relay::path_only(&forward_path)),
            ]
        )
    );

    // ── 构造转发请求：头部清洗 + Host 重写 + origin-form URI ──
    let (mut parts, body) = req.into_parts();
    parts.headers = http_relay::strip_hop_by_hop(&parts.headers);
    http_relay::set_host(
        &mut parts.headers,
        &http_relay::host_header(&target.host, target.port, target.default_port()),
    );
    parts.version = http::Version::HTTP_11;
    parts.uri = match forward_path.parse::<Uri>() {
        Ok(uri) => uri,
        Err(_) => return Ok(text_response(StatusCode::BAD_REQUEST, "Bad request URI")),
    };

    // ── 转发 ──
    let start = Instant::now();
    let outbound = Request::from_parts(parts, body);
    match send_upstream(
        &rule.upstream,
        target.use_tls,
        &target.host,
        target.port,
        outbound,
    )
    .await
    {
        Ok(resp) => {
            let is_sse = http_relay::is_event_stream(resp.headers());
            tracing::info!(
                rule_id = %rule.id,
                "{}",
                crate::i18n::tr_args(
                    "log.response",
                    &[
                        ("status", resp.status().as_str()),
                        ("dur", &format_duration(start.elapsed())),
                        ("extra", if is_sse { " · SSE" } else { "" }),
                    ],
                )
            );
            Ok(relay_response(resp))
        }
        Err(e) => {
            stats.error();
            tracing::warn!(
                rule_id = %rule.id,
                "upstream request failed for {}:{}: {e}",
                target.host,
                target.port
            );
            Ok(text_response(StatusCode::BAD_GATEWAY, "Bad Gateway"))
        }
    }
}

fn log_auth_failure(rule: &Rule, peer_addr: SocketAddr) {
    tracing::warn!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args("log.auth_failed", &[("peer", &peer_addr.to_string())])
    );
}

// ─────────────── 目标解析 ───────────────

/// 拆开的目标地址。
#[derive(Debug, PartialEq, Eq)]
struct Target {
    use_tls: bool,
    host: String,
    port: u16,
    /// 目标 URL 自带的基路径，已去掉末尾斜杠；无基路径时为空串。
    base_path: String,
}

impl Target {
    fn default_port(&self) -> u16 {
        if self.use_tls {
            443
        } else {
            80
        }
    }
}

/// 解析 `http(s)://host[:port][/base]`。缺少 scheme 时返回 `None`（配置校验也会拦）。
fn parse_target(url: &str) -> Option<Target> {
    let (use_tls, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, url.strip_prefix("http://")?),
    };

    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }

    let default_port = if use_tls { 443 } else { 80 };
    let (host, port) = crate::config::parse_host_port(authority)
        .map(|(h, p)| (h.to_owned(), p))
        .unwrap_or_else(|_| (authority.to_owned(), default_port));

    Some(Target {
        use_tls,
        host,
        port,
        base_path: path.trim_end_matches('/').to_owned(),
    })
}

/// 把目标基路径接在请求路径前面。`base` 要么为空，要么以 `/` 开头且不以 `/` 结尾。
fn join_path(base: &str, rest: &str) -> String {
    if base.is_empty() {
        rest.to_owned()
    } else if rest == "/" {
        base.to_owned()
    } else {
        format!("{base}{rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_host() {
        let t = parse_target("https://api.anthropic.com").unwrap();
        assert_eq!(
            t,
            Target {
                use_tls: true,
                host: "api.anthropic.com".to_owned(),
                port: 443,
                base_path: String::new(),
            }
        );
    }

    #[test]
    fn parses_port_and_base_path() {
        let t = parse_target("http://10.0.0.5:8000/api/").unwrap();
        assert!(!t.use_tls);
        assert_eq!(t.host, "10.0.0.5");
        assert_eq!(t.port, 8000);
        assert_eq!(t.base_path, "/api");
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(parse_target("api.anthropic.com").is_none());
        assert!(parse_target("https://").is_none());
    }

    #[test]
    fn joins_base_path() {
        assert_eq!(join_path("", "/v1/messages"), "/v1/messages");
        assert_eq!(join_path("/api", "/v1/messages"), "/api/v1/messages");
        assert_eq!(join_path("/api", "/"), "/api");
        // 查询串跟着路径一起过去
        assert_eq!(join_path("/api", "/v1?stream=true"), "/api/v1?stream=true");
    }
}

//! 正向代理处理器。
//!
//! 支持三种协议，由第一个字节决定：
//!
//! - `0x05` → SOCKS5 服务端
//! - `CONNECT ` → HTTP CONNECT 隧道
//! - 其他 HTTP 方法 → 绝对形式 HTTP 代理
//!
//! 嗅探读走的那个字节会被 [`PeekStream`] 原样补回，后续解析看到的仍是完整的流。
//! 处理器对入站流类型泛型，因此明文入站和 TLS 入站（握手在 [`listener`] 层完成）
//! 走的是同一套代码。
//!
//! [`listener`]: crate::engine::listener

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use http::{Method, Request, Response, StatusCode, Uri};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::Rule;
use crate::engine::auth;
use crate::engine::http_relay::{
    self, empty_body, relay_response, send_upstream, text_response, BoxError, ProxyBody,
};
use crate::engine::upstream;
use crate::status::Stats;

// ─────────────── 公开入口 ───────────────

/// 处理一条正向代理连接。`stream` 已是明文（TLS 入站在上层解密）。
pub async fn handle_forward<S>(
    mut stream: S,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> Result<(), BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 读一个字节判协议，随后用 PeekStream 还回去。
    let mut first = [0u8; 1];
    if stream.read(&mut first).await? == 0 {
        return Ok(()); // 客户端连上就关，正常收尾
    }
    let stream = PeekStream::with_byte(stream, first[0]);

    if first[0] == 0x05 {
        handle_socks5(stream, peer_addr, rule, stats).await
    } else {
        handle_http_proxy(stream, peer_addr, rule, stats).await
    }
}

// ─────────────── PeekStream ───────────────

/// 把嗅探时读走的首字节补回流首。
pub struct PeekStream<S> {
    inner: S,
    peeked: Option<u8>,
}

impl<S> PeekStream<S> {
    /// 用已读出的首字节构造：下一次读会先拿到这个字节。
    pub fn with_byte(inner: S, byte: u8) -> Self {
        Self {
            inner,
            peeked: Some(byte),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PeekStream<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.peeked.is_some() {
            // 容量为 0 时不能取出，否则字节会丢；直接返回等调用方给缓冲。
            if buf.remaining() == 0 {
                return std::task::Poll::Ready(Ok(()));
            }
            let byte = self.peeked.take().unwrap_or_default();
            buf.put_slice(&[byte]);
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PeekStream<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ─────────────── HTTP 代理 ───────────────

async fn handle_http_proxy<S>(
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

    hyper::server::conn::http1::Builder::new()
        .serve_connection(
            TokioIo::new(stream),
            service_fn(move |req: Request<Incoming>| {
                let rule = Arc::clone(&rule);
                let stats = Arc::clone(&stats);
                async move { serve_http_proxy(req, peer_addr, rule, stats).await }
            }),
        )
        .with_upgrades()
        .await?;
    Ok(())
}

async fn serve_http_proxy(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
) -> Result<Response<ProxyBody>, std::convert::Infallible> {
    // ── 代理认证 ──
    let credential = req
        .headers()
        .get(http::header::PROXY_AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !auth::check_basic(&rule.auth, credential).is_ok() {
        stats.auth_fail();
        tracing::warn!(
            rule_id = %rule.id,
            "{}",
            crate::i18n::tr_args("log.auth_failed", &[("peer", &peer_addr.to_string())])
        );
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

    if req.method() == Method::CONNECT {
        Ok(handle_connect(req, peer_addr, rule, stats).await)
    } else {
        Ok(handle_absolute_form(req, rule, stats).await)
    }
}

/// CONNECT 隧道。先拨通上游再回 200，避免客户端在一个注定失败的隧道里发数据。
async fn handle_connect(
    mut req: Request<Incoming>,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
) -> Response<ProxyBody> {
    let Some((host, port)) = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .and_then(parse_authority)
    else {
        return text_response(StatusCode::BAD_REQUEST, "Bad CONNECT target");
    };

    let target = format!("{host}:{port}");
    let conn = match upstream::dial(&rule.upstream, &host, port).await {
        Ok(c) => c,
        Err(e) => {
            stats.error();
            tracing::warn!(rule_id = %rule.id, "upstream dial failed for {target}: {e}");
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway");
        }
    };

    tracing::info!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.conn_open",
            &[("peer", &peer_addr.to_string()), ("target", &target)]
        )
    );

    let upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        match upgrade.await {
            Ok(upgraded) => {
                let mut client = TokioIo::new(upgraded);
                let mut conn = conn;
                let start = Instant::now();
                let (up, down) = copy_both(&mut client, &mut conn).await;
                // CountingStream 在 hyper 升级后不再覆盖隧道字节，
                // 需要在此单独累加，否则隧道流量不进统计。
                stats.add_bytes(up, down);
                tracing::info!(
                    rule_id = %rule.id,
                    "{}",
                    crate::i18n::tr_args(
                        "log.tunnel_close",
                        &[
                            ("up", &format_bytes(up)),
                            ("down", &format_bytes(down)),
                            ("dur", &format_duration(start.elapsed())),
                        ],
                    )
                );
            }
            Err(e) => {
                stats.error();
                tracing::debug!(rule_id = %rule.id, "CONNECT upgrade failed: {e}");
            }
        }
    });

    // 200 的响应体必须为空，其后的字节属于隧道。
    Response::new(empty_body())
}

/// 绝对形式请求（`GET http://host/path HTTP/1.1`）：改写成 origin-form 转给上游。
async fn handle_absolute_form(
    req: Request<Incoming>,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
) -> Response<ProxyBody> {
    let uri = req.uri().clone();
    let Some(authority) = uri.authority().cloned() else {
        // 没有 authority 说明这不是代理请求，而是把代理端口当普通服务器用了。
        return text_response(StatusCode::BAD_REQUEST, "Absolute-form URI required");
    };

    let use_tls = uri.scheme_str().is_some_and(|s| s.eq_ignore_ascii_case("https"));
    let default_port = if use_tls { 443 } else { 80 };
    let host = authority.host().to_owned();
    let port = authority.port_u16().unwrap_or(default_port);
    let path = uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());

    let method = req.method().clone();
    tracing::debug!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.request",
            &[("method", method.as_str()), ("path", http_relay::path_only(&path))]
        )
    );

    let (mut parts, body) = req.into_parts();
    parts.headers = http_relay::strip_hop_by_hop(&parts.headers);
    http_relay::set_host(
        &mut parts.headers,
        &http_relay::host_header(&host, port, default_port),
    );
    parts.version = http::Version::HTTP_11;
    parts.uri = match path.parse::<Uri>() {
        Ok(u) => u,
        Err(_) => return text_response(StatusCode::BAD_REQUEST, "Bad request URI"),
    };

    let start = Instant::now();
    let outbound = Request::from_parts(parts, body);
    match send_upstream(&rule.upstream, use_tls, &host, port, outbound).await {
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
            relay_response(resp)
        }
        Err(e) => {
            stats.error();
            tracing::warn!(rule_id = %rule.id, "upstream request failed for {host}:{port}: {e}");
            text_response(StatusCode::BAD_GATEWAY, "Bad Gateway")
        }
    }
}

// ─────────────── SOCKS5 ───────────────

/// RFC 1928 服务端，只支持 CONNECT；认证走 RFC 1929 用户名密码。
async fn handle_socks5<S>(
    mut stream: S,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> Result<(), BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // ── 阶段 1：方法协商 ──
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err("invalid SOCKS5 version".into());
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods).await?;

    let credentials = auth::socks5_credentials(&rule.auth);
    let wanted = if credentials.is_some() { 0x02 } else { 0x00 };
    if !methods.contains(&wanted) {
        stream.write_all(&[0x05, 0xFF]).await?;
        shutdown_after_reply(&mut stream).await;
        return Err(format!("no acceptable SOCKS5 auth method from {peer_addr}").into());
    }
    stream.write_all(&[0x05, wanted]).await?;

    // ── 阶段 2：用户名密码子协商（RFC 1929）──
    if wanted == 0x02 {
        let mut ver = [0u8; 1];
        stream.read_exact(&mut ver).await?;
        if ver[0] != 0x01 {
            return Err("invalid SOCKS5 auth sub-negotiation version".into());
        }

        let mut len = [0u8; 1];
        stream.read_exact(&mut len).await?;
        let mut user = vec![0u8; len[0] as usize];
        stream.read_exact(&mut user).await?;

        stream.read_exact(&mut len).await?;
        let mut pass = vec![0u8; len[0] as usize];
        stream.read_exact(&mut pass).await?;

        if !auth::check_socks5(&rule.auth, &user, &pass).is_ok() {
            stats.auth_fail();
            tracing::warn!(
                rule_id = %rule.id,
                "{}",
                crate::i18n::tr_args("log.auth_failed", &[("peer", &peer_addr.to_string())])
            );
            stream.write_all(&[0x01, 0x01]).await?;
            shutdown_after_reply(&mut stream).await;
            return Err(format!("SOCKS5 auth failed from {peer_addr}").into());
        }
        stream.write_all(&[0x01, 0x00]).await?;
    }

    // ── 阶段 3：请求 ──
    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    if req[0] != 0x05 {
        return Err("invalid SOCKS5 request version".into());
    }

    let (host, port) = match req[3] {
        0x01 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets).await?;
            (std::net::Ipv4Addr::from(octets).to_string(), read_port(&mut stream).await?)
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name).await?;
            (
                String::from_utf8_lossy(&name).into_owned(),
                read_port(&mut stream).await?,
            )
        }
        0x04 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            (std::net::Ipv6Addr::from(octets).to_string(), read_port(&mut stream).await?)
        }
        atyp => {
            // 地址长度取决于 atyp，认不出来就无从跳过后续字节，只能连同应答一起收摊。
            socks5_reply(&mut stream, 0x08).await?; // address type not supported
            shutdown_after_reply(&mut stream).await;
            return Err(format!("unsupported SOCKS5 atyp 0x{atyp:02x}").into());
        }
    };

    // ── 阶段 3.5：拒绝非 CONNECT 命令 ──
    //
    // 这一步必须排在地址读完之后：应答写进内核发送缓冲后若立刻关闭套接字，而接收
    // 缓冲里还压着没读走的地址字节，Windows 会以 RST 收尾（Linux 同样如此），
    // 客户端连同应答一起丢掉，只看到"连接被重置"。读完整条请求再答，才能正常 FIN。
    if req[1] != 0x01 {
        socks5_reply(&mut stream, 0x07).await?; // command not supported
        shutdown_after_reply(&mut stream).await;
        return Err(format!("unsupported SOCKS5 cmd 0x{:02x} from {peer_addr}", req[1]).into());
    }

    // ── 阶段 4：拨号后再回应答 ──
    let target = format!("{host}:{port}");
    let mut conn = match upstream::dial(&rule.upstream, &host, port).await {
        Ok(c) => c,
        Err(e) => {
            stats.error();
            socks5_reply(&mut stream, 0x05).await?; // connection refused
            shutdown_after_reply(&mut stream).await;
            return Err(format!("upstream dial failed for {target}: {e}").into());
        }
    };
    socks5_reply(&mut stream, 0x00).await?;

    tracing::info!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.conn_open",
            &[("peer", &peer_addr.to_string()), ("target", &target)]
        )
    );

    let start = Instant::now();
    let (up, down) = copy_both(&mut stream, &mut conn).await;
    tracing::info!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.tunnel_close",
            &[
                ("up", &format_bytes(up)),
                ("down", &format_bytes(down)),
                ("dur", &format_duration(start.elapsed())),
            ],
        )
    );
    Ok(())
}

/// 写完拒绝应答后优雅收尾：先刷缓冲，再关闭写方向发出 FIN。
///
/// 少了这一步就直接 drop 套接字，应答虽已写进发送缓冲，却可能被随后的 RST 一起
/// 作废——客户端看到的是"连接被重置"，而不是我们想告诉它的那个错误码。
async fn shutdown_after_reply<S>(stream: &mut S)
where
    S: AsyncWrite + Unpin,
{
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// SOCKS5 应答。绑定地址填 `0.0.0.0:0`——客户端只看状态码，不用这个地址。
async fn socks5_reply<S>(stream: &mut S, code: u8) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
}

async fn read_port<S>(stream: &mut S) -> std::io::Result<u16>
where
    S: AsyncRead + Unpin,
{
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    Ok(u16::from_be_bytes(port))
}

// ─────────────── 辅助 ───────────────

/// 双向复制，返回 `(上行, 下行)`。
/// 出错时返回 `(0, 0)`——`copy_bidirectional` 不暴露部分计数，隧道按正常关闭处理。
async fn copy_both<A, B>(a: &mut A, b: &mut B) -> (u64, u64)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(a, b).await.unwrap_or_default()
}

fn parse_authority(authority: &str) -> Option<(String, u16)> {
    crate::config::parse_host_port(authority)
        .ok()
        .map(|(host, port)| (host.to_owned(), port))
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{:.1}MB", n as f64 / 1024.0 / 1024.0)
    }
}

pub fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn peek_stream_replays_first_byte() {
        let (mut client, server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            client.write_all(b"\x05\x01\x00").await.unwrap();
        });

        let mut server = server;
        let mut first = [0u8; 1];
        server.read_exact(&mut first).await.unwrap();
        assert_eq!(first[0], 0x05);

        // 嗅探读走的字节必须还回来，否则 SOCKS5 会丢掉版本号
        let mut peeked = PeekStream::with_byte(server, first[0]);
        let mut all = [0u8; 3];
        peeked.read_exact(&mut all).await.unwrap();
        assert_eq!(&all, b"\x05\x01\x00");
    }

    #[test]
    fn format_helpers() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2048), "2.0KB");
        assert_eq!(format_duration(std::time::Duration::from_millis(250)), "250ms");
        assert_eq!(format_duration(std::time::Duration::from_millis(1500)), "1.5s");
    }

    // ── format_bytes 边界值 ──────────────────────────────────

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0B");
    }

    #[test]
    fn format_bytes_just_below_kb() {
        assert_eq!(format_bytes(1023), "1023B");
    }

    #[test]
    fn format_bytes_exactly_1kb() {
        // 1024 正好跨越阈值，进入 KB 段
        assert_eq!(format_bytes(1024), "1.0KB");
    }

    #[test]
    fn format_bytes_just_below_mb() {
        // 1 MB - 1 byte 仍显示 KB
        let just_under = 1024 * 1024 - 1;
        assert!(format_bytes(just_under).ends_with("KB"),
            "expected KB suffix, got {}", format_bytes(just_under));
    }

    #[test]
    fn format_bytes_exactly_1mb() {
        assert_eq!(format_bytes(1024 * 1024), "1.0MB");
    }

    #[test]
    fn format_bytes_large_value() {
        // 10.5 MB
        assert_eq!(format_bytes(10 * 1024 * 1024 + 512 * 1024), "10.5MB");
    }

    // ── format_duration 边界值 ───────────────────────────────

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration(std::time::Duration::from_millis(0)), "0ms");
    }

    #[test]
    fn format_duration_just_below_1s() {
        assert_eq!(format_duration(std::time::Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_duration_exactly_1s() {
        assert_eq!(format_duration(std::time::Duration::from_millis(1000)), "1.0s");
    }

    #[test]
    fn format_duration_large() {
        assert_eq!(format_duration(std::time::Duration::from_millis(12500)), "12.5s");
    }

    // ── counter 补充：初始值为零 ─────────────────────────────

    #[test]
    fn byte_counters_initial_zero() {
        use crate::engine::counter::ByteCounters;
        let c = ByteCounters::new();
        assert_eq!(c.get(), (0, 0));
    }

    #[tokio::test]
    async fn counting_stream_accumulates_across_calls() {
        use crate::engine::counter::{ByteCounters, CountingStream};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(256);
        let counters = ByteCounters::new();
        let mut counted = CountingStream::new(server, counters.clone());

        // 两次写入
        client.write_all(b"aaa").await.unwrap();
        client.write_all(b"bb").await.unwrap();

        let mut buf = [0u8; 5];
        counted.read_exact(&mut buf).await.unwrap();

        // 两次写回
        counted.write_all(b"x").await.unwrap();
        counted.write_all(b"yz").await.unwrap();

        // up = 5（读了 5 字节），down = 3（写了 3 字节）
        assert_eq!(counters.get(), (5, 3));
    }
}

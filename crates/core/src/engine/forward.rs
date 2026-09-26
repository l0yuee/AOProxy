//! 正向代理处理器。
//!
//! 支持三种协议，由第一个字节决定：
//!
//! - `0x05` → SOCKS5 服务端
//! - `CONNECT ` → HTTP CONNECT 隧道
//! - 其他 HTTP 方法 → 绝对形式 HTTP 代理
//!
//! 第一个字节由 [`listener`] 读出（它顺带负责「迟迟不发数据」的超时），装在
//! [`PeekStream`] 里交过来，后续解析看到的仍是完整的流。处理器对入站流类型泛型，
//! 因此明文入站和 TLS 入站（握手也在 [`listener`] 层完成）走的是同一套代码。
//!
//! [`listener`]: crate::engine::listener

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use http::{Method, Request, Response, StatusCode, Uri};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::Rule;
use crate::engine::auth;
use crate::engine::counter::{ByteCounters, CountingStream};
use crate::engine::http_relay::{
    self, empty_body, relay_response, send_upstream, text_response, BoxError, ProxyBody,
};
use crate::engine::listener::HANDSHAKE_TIMEOUT;
use crate::engine::upstream::{self, UpstreamConn};
use crate::status::Stats;

// ─────────────── 公开入口 ───────────────

/// 处理一条正向代理连接。`stream` 已是明文（TLS 入站在上层解密），首字节已读出待还。
pub async fn handle_forward<S>(
    stream: PeekStream<S>,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> Result<(), BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if stream.peeked() == Some(0x05) {
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

    /// 还没被读走的那个首字节。
    pub fn peeked(&self) -> Option<u8> {
        self.peeked
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

/// CONNECT 已拨通上游、等 hyper 把客户端连接交出来的隧道。
struct Tunnel {
    upgrade: hyper::upgrade::OnUpgrade,
    conn: UpstreamConn,
    target: String,
}

/// 服务函数与连接任务之间交接隧道的槽位。一条连接升级之后就不再有下一个请求，
/// 所以至多装一条。
type TunnelSlot = Arc<Mutex<Option<Tunnel>>>;

async fn handle_http_proxy<S>(
    stream: S,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> Result<(), BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let slot = TunnelSlot::default();
    let service = {
        let rule = Arc::clone(rule);
        let stats = Arc::clone(stats);
        let slot = Arc::clone(&slot);
        service_fn(move |req: Request<Incoming>| {
            let rule = Arc::clone(&rule);
            let stats = Arc::clone(&stats);
            let slot = Arc::clone(&slot);
            async move { serve_http_proxy(req, peer_addr, rule, stats, slot).await }
        })
    };

    hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HANDSHAKE_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await?;

    // CONNECT 升级后，隧道就在这条连接自己的任务里接着跑，而不是另起任务：
    // 连接的结算（活跃数减一、线路字节入账）在本函数返回之后才做，隧道若跑在
    // 别处，结算就发生在隧道还开着的时候——活跃数提前归零，隧道流量只能另记一笔，
    // 遇上 RST 还会整段丢失。
    let tunnel = slot.lock().take();
    if let Some(tunnel) = tunnel {
        run_tunnel(tunnel, &rule.id, stats).await;
    }
    Ok(())
}

async fn serve_http_proxy(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
    slot: TunnelSlot,
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
        Ok(handle_connect(req, peer_addr, rule, stats, slot).await)
    } else {
        Ok(handle_absolute_form(req, rule, stats).await)
    }
}

/// CONNECT 隧道。先拨通上游再回 200，避免客户端在一个注定失败的隧道里发数据。
///
/// 隧道本身不在这里跑：200 发出去之前 hyper 不会交出连接，所以把拨通的上游
/// 留在 `slot` 里，由连接任务在 hyper 收尾后接手（见 [`handle_http_proxy`]）。
async fn handle_connect(
    mut req: Request<Incoming>,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
    slot: TunnelSlot,
) -> Response<ProxyBody> {
    let Some((host, port)) = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .and_then(parse_authority)
    else {
        return text_response(StatusCode::BAD_REQUEST, "Bad CONNECT target");
    };

    let target = crate::config::join_host_port(&host, port);
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

    *slot.lock() = Some(Tunnel {
        upgrade: hyper::upgrade::on(&mut req),
        conn,
        target,
    });

    // 200 的响应体必须为空，其后的字节属于隧道。
    Response::new(empty_body())
}

/// 等 hyper 交出客户端连接，然后双向转发到隧道关闭。
///
/// 隧道里的字节不在这里入账：升级后的连接底下仍是监听器包的那层计数流，
/// 连接结算时一并算上，这里再记就重复了。
async fn run_tunnel(tunnel: Tunnel, rule_id: &str, stats: &Stats) {
    let upgraded = match tunnel.upgrade.await {
        Ok(upgraded) => upgraded,
        Err(e) => {
            stats.error();
            tracing::debug!(
                rule_id = %rule_id,
                "CONNECT upgrade failed for {}: {e}",
                tunnel.target
            );
            return;
        }
    };

    let start = Instant::now();
    let (up, down) = copy_both(&mut TokioIo::new(upgraded), tunnel.conn).await;
    tracing::info!(
        rule_id = %rule_id,
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
    // `Authority::host()` 给 IPv6 字面量留着方括号（`[::1]`），那是 URL 的语法；
    // 拨号、SNI 与 `host_header` 要的都是裸地址。
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
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
            tracing::warn!(
                rule_id = %rule.id,
                "upstream request failed for {}: {e}",
                crate::config::join_host_port(&host, port)
            );
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
    // 协商限时，拨号与隧道不限：前者由客户端掌控节奏，拖着不说完就一直占着连接。
    let (host, port) = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        socks5_negotiate(&mut stream, peer_addr, rule, stats),
    )
    .await
    .map_err(|_| BoxError::from(format!("SOCKS5 negotiation with {peer_addr} timed out")))??;

    // ── 阶段 4：拨号后再回应答 ──
    let target = crate::config::join_host_port(&host, port);
    let conn = match upstream::dial(&rule.upstream, &host, port).await {
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
    let (up, down) = copy_both(&mut stream, conn).await;
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

/// SOCKS5 协商：方法选择、用户名密码子协商、CONNECT 请求，返回客户端要连的目标。
///
/// 协商失败时已经回过拒绝应答并关闭了写方向，调用方只需收尾。
async fn socks5_negotiate<S>(
    stream: &mut S,
    peer_addr: SocketAddr,
    rule: &Rule,
    stats: &Stats,
) -> Result<(String, u16), BoxError>
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
        shutdown_after_reply(stream).await;
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
            shutdown_after_reply(stream).await;
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
            (std::net::Ipv4Addr::from(octets).to_string(), read_port(stream).await?)
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name).await?;
            (
                String::from_utf8_lossy(&name).into_owned(),
                read_port(stream).await?,
            )
        }
        0x04 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            (std::net::Ipv6Addr::from(octets).to_string(), read_port(stream).await?)
        }
        atyp => {
            // 地址长度取决于 atyp，认不出来就无从跳过后续字节，只能连同应答一起收摊。
            socks5_reply(stream, 0x08).await?; // address type not supported
            shutdown_after_reply(stream).await;
            return Err(format!("unsupported SOCKS5 atyp 0x{atyp:02x}").into());
        }
    };

    // ── 阶段 3.5：拒绝非 CONNECT 命令 ──
    //
    // 这一步必须排在地址读完之后：应答写进内核发送缓冲后若立刻关闭套接字，而接收
    // 缓冲里还压着没读走的地址字节，Windows 会以 RST 收尾（Linux 同样如此），
    // 客户端连同应答一起丢掉，只看到"连接被重置"。读完整条请求再答，才能正常 FIN。
    if req[1] != 0x01 {
        socks5_reply(stream, 0x07).await?; // command not supported
        shutdown_after_reply(stream).await;
        return Err(format!("unsupported SOCKS5 cmd 0x{:02x} from {peer_addr}", req[1]).into());
    }

    Ok((host, port))
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

/// 双向复制到任一端关闭，返回 `(上行, 下行)`：客户端→上游、上游→客户端各送出多少字节。
///
/// 隧道以错误收场（多半是某一端被重置）时也返回已经送出的部分。`copy_bidirectional`
/// 出错时不给计数，所以在上游那一侧套一层计数流自己记。
async fn copy_both<A, B>(client: &mut A, upstream: B) -> (u64, u64)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let counters = ByteCounters::new();
    let mut upstream = CountingStream::new(upstream, counters.clone());
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
    // 计数流的「上/下」是站在被包的那一端说的：从上游读出的是隧道的下行，
    // 写进上游的才是上行，这里要对调。
    let (from_upstream, to_upstream) = counters.get();
    (to_upstream, from_upstream)
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

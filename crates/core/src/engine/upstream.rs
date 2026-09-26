//! 上游连接拨号。
//!
//! 将下游请求通过配置的上游代理转发到目标地址，支持四种上游类型：
//!
//! - `direct`：直接 TCP 连接目标。
//! - `http`：通过 HTTP CONNECT 隧道。
//! - `https`：通过 HTTPS CONNECT 隧道（TLS 连接到代理）。
//! - `socks5`：通过 SOCKS5 代理（支持用户名密码认证，远端 DNS 解析）。

use std::net::IpAddr;
use std::sync::Arc;

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::config::{join_host_port, Upstream, UpstreamKind};
use crate::error::{Error, Result};

/// 上游拨号结果：实现了 [`AsyncRead`] + [`AsyncWrite`] 的连接。
/// 用 enum 替代 Box<dyn…> 以避免堆分配（常见路径是 Direct 和 Socks5）。
pub enum UpstreamConn {
    Tcp(TcpStream),
    /// rustls 的会话状态有数 KB，直接内联会把整个 enum 撑到那个尺寸，
    /// 连 Direct 路径也要按最大变体搬运，故装箱。
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
    /// 已建立的上游连接之上再套一层 TLS，用于 `wrap_tls`。
    /// 内层可以是任意 UpstreamConn（直连、HTTP CONNECT 隧道、SOCKS5），故递归装箱。
    Nested(Box<tokio_rustls::client::TlsStream<UpstreamConn>>),
}

impl AsyncRead for UpstreamConn {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
            Self::Nested(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for UpstreamConn {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
            Self::Nested(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
            Self::Nested(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
            Self::Nested(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// 拨通到 `target_host:target_port` 的连接，路由由 `upstream` 配置决定。
///
/// - 上游日志（"经上游 socks5 …"）由调用方记录，本函数只负责建立连接。
pub async fn dial(
    upstream: &Upstream,
    target_host: &str,
    target_port: u16,
) -> Result<UpstreamConn> {
    match upstream.kind {
        UpstreamKind::Direct => dial_direct(target_host, target_port).await,
        UpstreamKind::Http => {
            let (proxy_host, proxy_port) = upstream.host_port()?;
            dial_http_connect(proxy_host, proxy_port, target_host, target_port, &upstream.username, &upstream.password, false).await
        }
        UpstreamKind::Https => {
            let (proxy_host, proxy_port) = upstream.host_port()?;
            let owned_host = proxy_host.to_owned();
            dial_http_connect(&owned_host, proxy_port, target_host, target_port, &upstream.username, &upstream.password, true).await
        }
        UpstreamKind::Socks5 => {
            let (proxy_host, proxy_port) = upstream.host_port()?;
            let owned_host = proxy_host.to_owned();
            dial_socks5(
                &owned_host,
                proxy_port,
                target_host,
                target_port,
                upstream.username.as_deref(),
                upstream.password.as_deref(),
            )
            .await
        }
    }
}

// ─────────────── 直连 ───────────────

async fn dial_direct(host: &str, port: u16) -> Result<UpstreamConn> {
    let stream = TcpStream::connect((host, port)).await.map_err(|e| {
        Error::UpstreamUnreachable {
            address: join_host_port(host, port),
            source: e,
        }
    })?;
    stream.set_nodelay(true).ok();
    Ok(UpstreamConn::Tcp(stream))
}

// ─────────────── HTTP CONNECT ───────────────

async fn dial_http_connect(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    username: &Option<String>,
    password: &Option<String>,
    use_tls: bool,
) -> Result<UpstreamConn> {
    let proxy_address = join_host_port(proxy_host, proxy_port);
    let tcp = TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|e| Error::UpstreamUnreachable {
            address: proxy_address.clone(),
            source: e,
        })?;
    tcp.set_nodelay(true).ok();

    let mut conn: UpstreamConn = if use_tls {
        let tls_stream = upgrade_tls(tcp, proxy_host).await?;
        UpstreamConn::Tls(Box::new(tls_stream))
    } else {
        UpstreamConn::Tcp(tcp)
    };

    // 发送 CONNECT 请求。IPv6 字面量必须带方括号，否则 `::1:443` 分不出哪段是端口。
    let authority = join_host_port(target_host, target_port);
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
        let cred = base64::engine::general_purpose::STANDARD
            .encode(format!("{u}:{p}"));
        req.push_str(&format!("Proxy-Authorization: Basic {cred}\r\n"));
    }
    req.push_str("\r\n");

    use tokio::io::AsyncWriteExt;
    conn.write_all(req.as_bytes())
        .await
        .map_err(|e| Error::UpstreamUnreachable {
            address: proxy_address,
            source: e,
        })?;
    read_connect_response(&mut conn, &authority).await?;
    Ok(conn)
}

/// CONNECT 应答头的长度上限。正常的应答只有几十个字节，这个值只用来挡住
/// 不停发数据却始终不给空行的上游，免得缓冲无限长大。
const MAX_CONNECT_RESPONSE: usize = 8 * 1024;

/// 读 CONNECT 应答头，读到空行为止，并确认状态码是 200。
///
/// 逐字节读：应答头之后紧跟着的就是隧道数据——服务端先说话的协议（SSH、SMTP）
/// 会和应答挤在同一个包里到达，成块读会把它们一并吞进缓冲、随后丢掉。
/// 应答头只有几十个字节，逐字节读的代价可以忽略；TLS 上游读的是 rustls 已解密的
/// 缓冲，也不会每个字节都陷一次内核。
async fn read_connect_response<S>(stream: &mut S, target: &str) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut head = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < MAX_CONNECT_RESPONSE {
        if stream.read(&mut byte).await.map_err(Error::Io)? == 0 {
            break;
        }
        head.push(byte[0]);
    }

    match parse_connect_status(&head) {
        200 if head.ends_with(b"\r\n\r\n") => Ok(()),
        // 状态行是 200，应答头却没说完就断了或超长：隧道的起点无从确定。
        200 => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete CONNECT response from upstream proxy",
        ))),
        status => Err(Error::UpstreamConnectRejected {
            target: target.to_owned(),
            status,
        }),
    }
}

/// 取状态行里的状态码（`HTTP/1.1 200 Connection established` → 200），认不出来时为 0。
fn parse_connect_status(response: &[u8]) -> u16 {
    let text = String::from_utf8_lossy(response);
    text.lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ─────────────── SOCKS5 ───────────────

async fn dial_socks5(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<UpstreamConn> {
    use tokio_socks::tcp::Socks5Stream;

    let proxy_addr = join_host_port(proxy_host, proxy_port);

    // 判断 target 是 IP 还是域名，以便选对 tokio-socks API
    let stream = if let Ok(ip) = target_host.parse::<IpAddr>() {
        let target = std::net::SocketAddr::new(ip, target_port);
        match (username, password) {
            (Some(u), Some(p)) => {
                Socks5Stream::connect_with_password(&*proxy_addr, target, u, p)
                    .await
                    .map_err(Error::Socks)?
            }
            _ => Socks5Stream::connect(&*proxy_addr, target)
                .await
                .map_err(Error::Socks)?,
        }
    } else {
        // 域名：让代理做 DNS 解析（远端 DNS）
        let target = (target_host, target_port);
        match (username, password) {
            (Some(u), Some(p)) => {
                Socks5Stream::connect_with_password(&*proxy_addr, target, u, p)
                    .await
                    .map_err(Error::Socks)?
            }
            _ => Socks5Stream::connect(&*proxy_addr, target)
                .await
                .map_err(Error::Socks)?,
        }
    };

    let tcp: TcpStream = stream.into_inner();
    tcp.set_nodelay(true).ok();
    Ok(UpstreamConn::Tcp(tcp))
}

// ─────────────── TLS 升级 ───────────────

/// 进程级共享的客户端 TLS 配置。
///
/// 根证书列表有几百条，每次建连都重新解析会明显拖慢握手，这里只构建一次。
/// ALPN 固定为 http/1.1：上游请求由 hyper 的 http1 客户端发出，不宣告 h2
/// 可以避免目标站点协商到我们不会说的协议。
fn client_config() -> Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let mut cfg = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    }))
}

/// 在任意已建立的流之上完成 TLS 握手，SNI 取 `server_name`。
async fn upgrade_tls<S>(stream: S, server_name: &str) -> Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use rustls_pki_types::ServerName;
    use tokio_rustls::TlsConnector;

    let connector = TlsConnector::from(client_config());
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|_| Error::InvalidAddress(server_name.to_owned()))?;

    connector.connect(name, stream).await.map_err(Error::Io)
}

/// 给已拨通的上游连接再套一层 TLS，用于转发到 `https://` 目标。
///
/// 目标既可能是直连的站点，也可能在 CONNECT 隧道或 SOCKS5 隧道之后，
/// 故包一层 [`UpstreamConn::Nested`] 而不是限定内层类型。
pub async fn wrap_tls(conn: UpstreamConn, server_name: &str) -> Result<UpstreamConn> {
    let tls = upgrade_tls(conn, server_name).await?;
    Ok(UpstreamConn::Nested(Box::new(tls)))
}

/// UpstreamConn 的内部类型都是 Unpin，显式声明以满足 copy_bidirectional 的约束。
impl Unpin for UpstreamConn {}

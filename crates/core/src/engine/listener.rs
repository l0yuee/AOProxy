//! 监听器：绑定端口，接受连接，分发到正向或反向代理处理器。
//!
//! 绑定与服务分成两步：[`bind`] 是同步可失败的，调用方能当场拿到"端口被占用"
//! 这类错误；[`serve`] 才进入长跑的 accept 循环。若把两者合在一个 spawn 里，
//! 绑定失败就只能事后从日志里找。
//!
//! 每条连接都先包一层 [`CountingStream`]，TLS 握手（如有）在计数层之上完成，
//! 因此统计到的是线路上的字节数。

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::config::{Mode, Rule};
use crate::engine::counter::{ByteCounters, CountingStream};
use crate::engine::forward::PeekStream;
use crate::engine::tls::build_acceptor;
use crate::error::{Error, Result};
use crate::status::Stats;

/// 新连接完成握手的时限：TLS 握手、送来第一个字节、SOCKS5 协商、HTTP 请求头各自不得超过它。
///
/// 连上之后一声不吭的连接会一直占着一个文件描述符。公网端口上被人攒够一千来个，
/// 进程的描述符就用完了，正常客户端再也连不进来。60 秒与 nginx 的
/// `client_header_timeout`、`ssl_handshake_timeout` 默认值一致，正常客户端碰不到。
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// accept 因资源不足（描述符用尽、内存不足）失败后，歇多久再试。
/// 立刻重试只会在同一个错误上空转，把 CPU 和日志一起烧掉。
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

/// 已绑定好的监听资源，交给 [`serve`] 运行。
pub struct Bound {
    listener: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
}

impl Bound {
    /// 实际绑定到的地址。`listen` 写 `127.0.0.1:0` 时端口由内核分配，
    /// 只有绑定之后才知道它是几号——集成测试靠这个拿到端口再去连。
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// 绑定端口并准备 TLS acceptor。地址非法、端口被占用或证书读取失败都在这里暴露。
pub async fn bind(rule: &Rule) -> Result<Bound> {
    let addr = SocketAddr::from_str(&rule.listen).map_err(|_| Error::Bind {
        id: rule.id.clone(),
        addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid listen address"),
    })?;

    // 证书先于端口校验：拿不到证书就没必要占端口。
    let tls = rule.tls.as_ref().map(build_acceptor).transpose()?;

    let listener = TcpListener::bind(addr).await.map_err(|e| Error::Bind {
        id: rule.id.clone(),
        addr,
        source: e,
    })?;

    Ok(Bound { listener, tls })
}

/// 运行 accept 循环直到 `cancel` 被触发。
///
/// 取消只停止接受新连接，已建立的连接各自跑完——正在传输的 SSE 不会被拦腰截断。
///
/// accept 出错不会结束循环（见 [`is_connection_error`]），所以目前总是返回 `Ok`；
/// 签名里的 `Result` 留给将来真正无法恢复的错误。
pub async fn serve(
    bound: Bound,
    rule: Rule,
    stats: Arc<Stats>,
    cancel: CancellationToken,
) -> Result<()> {
    let Bound { listener, tls } = bound;

    tracing::info!(
        rule_id = %rule.id,
        "{}",
        crate::i18n::tr_args(
            "log.rule_listening",
            &[("addr", &rule.listen), ("detail", &rule.detail_str())]
        )
    );

    let rule = Arc::new(rule);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer_addr)) => {
                        let rule = Arc::clone(&rule);
                        let stats = Arc::clone(&stats);
                        let tls = tls.clone();
                        tokio::spawn(async move {
                            handle_conn(stream, peer_addr, rule, stats, tls).await;
                        });
                    }
                    // accept 出错不能结束循环：那样规则就永久停摆了，而进程还活着，
                    // systemd 也不会来重启它。连接在 accept 之前就被对端重置属于常态，
                    // 直接接着 accept；其余多半是描述符用尽（EMFILE / ENFILE）这类
                    // 资源不足，等占着的连接释放后自会恢复，歇一会儿再试。
                    Err(e) if is_connection_error(&e) => {
                        tracing::debug!(rule_id = %rule.id, "accept error: {e}");
                    }
                    Err(e) => {
                        tracing::warn!(rule_id = %rule.id, "accept error, retrying in 1s: {e}");
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(ACCEPT_BACKOFF) => {}
                        }
                    }
                }
            }
        }
    }

    tracing::info!(rule_id = %rule.id, "{}", crate::i18n::tr("log.rule_stopped"));
    Ok(())
}

async fn handle_conn(
    stream: TcpStream,
    peer_addr: SocketAddr,
    rule: Arc<Rule>,
    stats: Arc<Stats>,
    tls: Option<tokio_rustls::TlsAcceptor>,
) {
    stream.set_nodelay(true).ok();
    stats.conn_open();

    let counters = ByteCounters::new();
    let stream = CountingStream::new(stream, counters.clone());

    let result = match tls {
        Some(acceptor) => {
            let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                .await
                .unwrap_or_else(|_| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "TLS handshake timed out",
                    ))
                });
            match handshake {
                Ok(tls_stream) => dispatch(tls_stream, peer_addr, &rule, &stats).await,
                Err(e) => {
                    // 端口扫描、证书不被客户端接受都会走到这里，记 debug 即可。
                    tracing::debug!(
                        rule_id = %rule.id,
                        "TLS handshake failed from {peer_addr}: {e}"
                    );
                    stats.error();
                    let (up, down) = counters.get();
                    stats.conn_close(up, down);
                    return;
                }
            }
        }
        None => dispatch(stream, peer_addr, &rule, &stats).await,
    };

    if let Err(e) = result {
        tracing::debug!(rule_id = %rule.id, "connection error from {peer_addr}: {e}");
        stats.error();
    }

    let (up, down) = counters.get();
    stats.conn_close(up, down);
}

/// 按规则模式分发。明文流与 TLS 解密后的流走的是同一个函数。
///
/// 分发前先等客户端送来第一个字节（最多 [`HANDSHAKE_TIMEOUT`]），再用 [`PeekStream`]
/// 还回去：正向模式本来就要靠它判断协议；反向模式交给 hyper 之后，hyper 的请求头
/// 超时只从收到第一个字节才开始计，连上后一个字节都不发的连接得在这里挡。
async fn dispatch<S>(
    mut stream: S,
    peer_addr: SocketAddr,
    rule: &Arc<Rule>,
    stats: &Arc<Stats>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut first = [0u8; 1];
    let read = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.read(&mut first))
        .await
        .map_err(|_| "client sent nothing before the handshake timeout")??;
    if read == 0 {
        return Ok(()); // 客户端连上就关，正常收尾
    }
    let stream = PeekStream::with_byte(stream, first[0]);

    match rule.mode {
        Mode::Forward => crate::engine::forward::handle_forward(stream, peer_addr, rule, stats).await,
        Mode::Reverse => crate::engine::reverse::handle_reverse(stream, peer_addr, rule, stats).await,
    }
}

/// 连接在 accept 之前就已被对端放弃或重置：换下一个连接接着 accept 即可，不必退避。
fn is_connection_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionRefused
            | ErrorKind::Interrupted
            | ErrorKind::WouldBlock
    )
}

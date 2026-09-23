//! 端到端测试：真的绑端口、真的把协议走一遍。
//!
//! 单元测试覆盖的是解析与校验这类纯函数，握手顺序、头部清洗、隧道转发这些
//! 只有把字节送上线才能验证。这里的做法是：起一个只会回 200 的原始服务器
//! （或回显服务器）当上游，起一条真规则当代理，然后用手写的字节流当客户端。
//!
//! 监听地址一律写 `127.0.0.1:0`，端口由内核分配，靠 [`Bound::local_addr`] 取回，
//! 所以并行跑多少个用例都不会撞端口。

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use aoproxy_core::engine::listener;
use aoproxy_core::{AuthConfig, AuthKind, Mode, Rule, Stats, Upstream};

// ─────────────── 测试替身 ───────────────

/// 原始服务器收到的请求头原文，按到达顺序排列。
type Recorded = Arc<Mutex<Vec<String>>>;

/// 一个只会回固定 200 的上游：把每条请求的头部整段记下来供断言。
async fn spawn_origin() -> (SocketAddr, Recorded) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind origin");
    let addr = listener.local_addr().expect("origin addr");
    let recorded: Recorded = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                // 先记后回：测试读到响应时，头部一定已经入表。
                if let Some(head) = read_head(&mut stream).await {
                    sink.lock().push(head);
                }
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                    .await;
                let _ = stream.flush().await;
            });
        }
    });

    (addr, recorded)
}

/// 回显服务器：原样写回收到的字节，用来验证隧道双向通。
async fn spawn_echo() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind echo");
    let addr = listener.local_addr().expect("echo addr");

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    addr
}

/// 读到空行为止，返回请求头（或响应头）原文。逐字节读，测试里够用。
async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read(&mut byte).await.ok()? == 1 {
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
        if buf.len() > 8192 {
            break;
        }
    }
    None
}

// ─────────────── 规则与客户端 ───────────────

fn base_rule(mode: Mode) -> Rule {
    Rule {
        id: "e2e".to_owned(),
        name: "e2e".to_owned(),
        enabled: true,
        mode,
        listen: "127.0.0.1:0".to_owned(),
        target: None,
        upstream: Upstream::default(),
        tls: None,
        auth: AuthConfig::default(),
    }
}

fn basic_auth(user: &str, pass: &str) -> AuthConfig {
    AuthConfig {
        kind: AuthKind::Basic,
        username: Some(user.to_owned()),
        password: Some(pass.to_owned()),
        token: None,
    }
}

fn token_auth(token: &str) -> AuthConfig {
    AuthConfig {
        kind: AuthKind::Token,
        username: None,
        password: None,
        token: Some(token.to_owned()),
    }
}

/// 启动一条规则，返回真实监听地址、统计与取消令牌（用例结束时取消即停）。
async fn start(rule: Rule) -> (SocketAddr, Arc<Stats>, CancellationToken) {
    let bound = listener::bind(&rule).await.expect("bind rule");
    let addr = bound.local_addr().expect("rule addr");
    let stats = Arc::new(Stats::new());
    let cancel = CancellationToken::new();
    tokio::spawn(listener::serve(
        bound,
        rule,
        Arc::clone(&stats),
        cancel.clone(),
    ));
    (addr, stats, cancel)
}

/// 发一段原始请求，读回整条响应。请求里都带 `connection: close`，读到 EOF 即完整。
async fn round_trip(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect proxy");
    stream.write_all(request.as_bytes()).await.expect("write request");
    stream.flush().await.expect("flush");
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.expect("read response");
    String::from_utf8_lossy(&resp).into_owned()
}

/// 上游收到的第一条请求头，小写化后便于断言。
fn first_head(recorded: &Recorded) -> String {
    let guard = recorded.lock();
    assert!(!guard.is_empty(), "上游没有收到任何请求");
    guard[0].to_ascii_lowercase()
}

// ─────────────── 反向代理 ───────────────

/// 反向代理的核心契约：`Host` 换成目标的，路径照抄，而客户端发给 AI 服务的
/// 凭据（`Authorization` / `x-api-key`）必须原封不动地到达上游。
#[tokio::test]
async fn reverse_rewrites_host_and_passes_api_key_through() {
    let (origin, recorded) = spawn_origin().await;
    let mut rule = base_rule(Mode::Reverse);
    rule.target = Some(format!("http://{origin}"));
    let (proxy, stats, cancel) = start(rule).await;

    let resp = round_trip(
        proxy,
        "GET /v1/messages HTTP/1.1\r\n\
         host: proxy.local\r\n\
         authorization: Bearer sk-test\r\n\
         x-api-key: sk-test\r\n\
         connection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let head = first_head(&recorded);
    assert!(head.starts_with("get /v1/messages http/1.1"), "{head}");
    assert!(head.contains(&format!("host: {origin}")), "{head}");
    assert!(head.contains("authorization: bearer sk-test"), "{head}");
    assert!(head.contains("x-api-key: sk-test"), "{head}");

    let snap = stats.snapshot();
    assert_eq!(snap.total, 1);
    assert_eq!(snap.auth_failures, 0);

    cancel.cancel();
}

/// 目标自带基路径时，请求路径接在它后面。
#[tokio::test]
async fn reverse_prefixes_target_base_path() {
    let (origin, recorded) = spawn_origin().await;
    let mut rule = base_rule(Mode::Reverse);
    rule.target = Some(format!("http://{origin}/api"));
    let (proxy, _stats, cancel) = start(rule).await;

    let resp = round_trip(
        proxy,
        "GET /v1/messages?stream=true HTTP/1.1\r\nhost: p\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(
        first_head(&recorded).starts_with("get /api/v1/messages?stream=true http/1.1"),
        "{}",
        first_head(&recorded)
    );

    cancel.cancel();
}

/// 令牌命中就剥掉再转发；不命中回 404，且请求根本不会到上游。
#[tokio::test]
async fn reverse_strips_path_token_and_hides_mismatch_as_404() {
    let (origin, recorded) = spawn_origin().await;
    let mut rule = base_rule(Mode::Reverse);
    rule.target = Some(format!("http://{origin}"));
    rule.auth = token_auth("tok123");
    let (proxy, stats, cancel) = start(rule).await;

    let ok = round_trip(
        proxy,
        "GET /tok123/v1/messages HTTP/1.1\r\nhost: p\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(
        first_head(&recorded).starts_with("get /v1/messages http/1.1"),
        "{}",
        first_head(&recorded)
    );

    let bad = round_trip(
        proxy,
        "GET /wrong/v1/messages HTTP/1.1\r\nhost: p\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(bad.starts_with("HTTP/1.1 404"), "{bad}");
    // 令牌不对的请求不应越过代理
    assert_eq!(recorded.lock().len(), 1);
    assert_eq!(stats.snapshot().auth_failures, 1);

    cancel.cancel();
}

/// Basic 认证走 `Proxy-Authorization`：失败回 407 并带质询头，
/// 成功后这个头作为 hop-by-hop 被剥掉，不会泄给上游。
#[tokio::test]
async fn reverse_basic_auth_challenges_then_strips_proxy_authorization() {
    let (origin, recorded) = spawn_origin().await;
    let mut rule = base_rule(Mode::Reverse);
    rule.target = Some(format!("http://{origin}"));
    rule.auth = basic_auth("alice", "s3cret");
    let (proxy, stats, cancel) = start(rule).await;

    let denied = round_trip(proxy, "GET /v1 HTTP/1.1\r\nhost: p\r\nconnection: close\r\n\r\n").await;
    let lower = denied.to_ascii_lowercase();
    assert!(denied.starts_with("HTTP/1.1 407"), "{denied}");
    assert!(
        lower.contains("proxy-authenticate: basic realm=\"aoproxy\""),
        "{denied}"
    );
    assert!(recorded.lock().is_empty(), "未认证的请求不应到达上游");

    let cred = base64::engine::general_purpose::STANDARD.encode("alice:s3cret");
    let ok = round_trip(
        proxy,
        &format!(
            "GET /v1 HTTP/1.1\r\nhost: p\r\nproxy-authorization: Basic {cred}\r\nconnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(
        !first_head(&recorded).contains("proxy-authorization"),
        "代理自己的凭据泄给了上游"
    );
    assert_eq!(stats.snapshot().auth_failures, 1);

    cancel.cancel();
}

// ─────────────── 正向代理：HTTP ───────────────

/// 绝对形式请求要改写成 origin-form 再发给上游。
#[tokio::test]
async fn forward_absolute_form_reaches_origin() {
    let (origin, recorded) = spawn_origin().await;
    let (proxy, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let resp = round_trip(
        proxy,
        &format!(
            "GET http://{origin}/v1/models HTTP/1.1\r\nhost: {origin}\r\nconnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(
        first_head(&recorded).starts_with("get /v1/models http/1.1"),
        "{}",
        first_head(&recorded)
    );

    cancel.cancel();
}

/// 把代理端口当普通服务器用（origin-form 请求）应当被拒，而不是当成转发。
#[tokio::test]
async fn forward_rejects_origin_form_request() {
    let (proxy, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let resp = round_trip(proxy, "GET /v1/models HTTP/1.1\r\nhost: p\r\nconnection: close\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");

    cancel.cancel();
}

#[tokio::test]
async fn forward_http_proxy_requires_basic_auth() {
    let (origin, recorded) = spawn_origin().await;
    let mut rule = base_rule(Mode::Forward);
    rule.auth = basic_auth("alice", "s3cret");
    let (proxy, stats, cancel) = start(rule).await;

    let denied = round_trip(
        proxy,
        &format!("GET http://{origin}/v1 HTTP/1.1\r\nhost: {origin}\r\nconnection: close\r\n\r\n"),
    )
    .await;
    assert!(denied.starts_with("HTTP/1.1 407"), "{denied}");
    assert!(recorded.lock().is_empty());
    assert_eq!(stats.snapshot().auth_failures, 1);

    let cred = base64::engine::general_purpose::STANDARD.encode("alice:s3cret");
    let ok = round_trip(
        proxy,
        &format!(
            "GET http://{origin}/v1 HTTP/1.1\r\nhost: {origin}\r\nproxy-authorization: Basic {cred}\r\nconnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");

    cancel.cancel();
}

/// CONNECT 隧道：200 之后的字节属于隧道，双向都要通。
#[tokio::test]
async fn forward_connect_tunnel_relays_both_directions() {
    let echo = spawn_echo().await;
    let (proxy, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");
    s.write_all(format!("CONNECT {echo} HTTP/1.1\r\nhost: {echo}\r\n\r\n").as_bytes())
        .await
        .expect("write CONNECT");
    let head = read_head(&mut s).await.expect("read CONNECT response");
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    s.write_all(b"ping").await.expect("write tunnel");
    let mut back = [0u8; 4];
    s.read_exact(&mut back).await.expect("read tunnel");
    assert_eq!(&back, b"ping");

    cancel.cancel();
}

// ─────────────── 正向代理：SOCKS5 ───────────────

/// 完整走一遍 RFC 1928 + RFC 1929：方法协商 → 用户名密码 → CONNECT → 隧道。
#[tokio::test]
async fn forward_socks5_tunnel_with_username_password() {
    let echo = spawn_echo().await;
    let mut rule = base_rule(Mode::Forward);
    rule.auth = basic_auth("u", "p");
    let (proxy, _stats, cancel) = start(rule).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");

    // 方法协商：只报 0x02（用户名密码）
    s.write_all(&[0x05, 0x01, 0x02]).await.expect("greet");
    let mut two = [0u8; 2];
    s.read_exact(&mut two).await.expect("method reply");
    assert_eq!(two, [0x05, 0x02]);

    // RFC 1929 子协商
    s.write_all(&[0x01, 1, b'u', 1, b'p']).await.expect("auth");
    s.read_exact(&mut two).await.expect("auth reply");
    assert_eq!(two, [0x01, 0x00]);

    // CONNECT 127.0.0.1:echo_port（atyp 0x01）
    let mut req = vec![0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1];
    req.extend_from_slice(&echo.port().to_be_bytes());
    s.write_all(&req).await.expect("socks request");
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).await.expect("socks reply");
    assert_eq!(reply[0], 0x05);
    assert_eq!(reply[1], 0x00, "SOCKS5 应答码不为成功");

    s.write_all(b"pong").await.expect("write tunnel");
    let mut back = [0u8; 4];
    s.read_exact(&mut back).await.expect("read tunnel");
    assert_eq!(&back, b"pong");

    cancel.cancel();
}

/// 域名形式的目标（atyp 0x03）由出站侧解析。
#[tokio::test]
async fn forward_socks5_accepts_domain_atyp() {
    let echo = spawn_echo().await;
    let (proxy, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");
    s.write_all(&[0x05, 0x01, 0x00]).await.expect("greet");
    let mut two = [0u8; 2];
    s.read_exact(&mut two).await.expect("method reply");
    assert_eq!(two, [0x05, 0x00]);

    let name = b"127.0.0.1";
    let mut req = vec![0x05, 0x01, 0x00, 0x03, name.len() as u8];
    req.extend_from_slice(name);
    req.extend_from_slice(&echo.port().to_be_bytes());
    s.write_all(&req).await.expect("socks request");
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).await.expect("socks reply");
    assert_eq!(reply[1], 0x00);

    s.write_all(b"abc").await.expect("write tunnel");
    let mut back = [0u8; 3];
    s.read_exact(&mut back).await.expect("read tunnel");
    assert_eq!(&back, b"abc");

    cancel.cancel();
}

/// 密码不对时回 `[0x01, 0x01]`（RFC 1929 的失败应答），并计入认证失败。
#[tokio::test]
async fn forward_socks5_rejects_wrong_password() {
    let mut rule = base_rule(Mode::Forward);
    rule.auth = basic_auth("u", "p");
    let (proxy, stats, cancel) = start(rule).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");
    s.write_all(&[0x05, 0x01, 0x02]).await.expect("greet");
    let mut two = [0u8; 2];
    s.read_exact(&mut two).await.expect("method reply");
    assert_eq!(two, [0x05, 0x02]);

    s.write_all(&[0x01, 1, b'u', 1, b'X']).await.expect("auth");
    s.read_exact(&mut two).await.expect("auth reply");
    assert_eq!(two, [0x01, 0x01]);
    assert_eq!(stats.snapshot().auth_failures, 1);

    cancel.cancel();
}

/// 要求认证的规则不接受"无认证"方法，回 0xFF 让客户端自己放弃。
#[tokio::test]
async fn forward_socks5_refuses_no_auth_when_credentials_required() {
    let mut rule = base_rule(Mode::Forward);
    rule.auth = basic_auth("u", "p");
    let (proxy, _stats, cancel) = start(rule).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");
    s.write_all(&[0x05, 0x01, 0x00]).await.expect("greet");
    let mut two = [0u8; 2];
    s.read_exact(&mut two).await.expect("method reply");
    assert_eq!(two, [0x05, 0xFF]);

    cancel.cancel();
}

/// 只支持 CONNECT；BIND / UDP ASSOCIATE 回 0x07。
#[tokio::test]
async fn forward_socks5_rejects_unsupported_command() {
    let (proxy, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let mut s = TcpStream::connect(proxy).await.expect("connect proxy");
    s.write_all(&[0x05, 0x01, 0x00]).await.expect("greet");
    let mut two = [0u8; 2];
    s.read_exact(&mut two).await.expect("method reply");

    // cmd 0x02 = BIND
    s.write_all(&[0x05, 0x02, 0x00, 0x01, 127, 0, 0, 1, 0x00, 0x50])
        .await
        .expect("socks request");
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).await.expect("socks reply");
    assert_eq!(reply[1], 0x07);

    cancel.cancel();
}

// ─────────────── 生命周期 ───────────────

/// 端口被占用必须当场报错，而不是事后从日志里找。
#[tokio::test]
async fn bind_reports_port_in_use() {
    let (addr, _stats, cancel) = start(base_rule(Mode::Forward)).await;

    let mut second = base_rule(Mode::Forward);
    second.id = "second".to_owned();
    second.listen = addr.to_string();
    assert!(
        matches!(
            listener::bind(&second).await,
            Err(aoproxy_core::Error::Bind { .. })
        ),
        "占用端口的第二次绑定应当失败"
    );

    cancel.cancel();
}

/// 取消只停止接受新连接：令牌取消后端口上再也连不上。
#[tokio::test]
async fn cancel_stops_accepting_new_connections() {
    let echo = spawn_echo().await;
    let mut rule = base_rule(Mode::Reverse);
    rule.target = Some(format!("http://{echo}"));
    let (proxy, _stats, cancel) = start(rule).await;

    // 先确认端口确实在服务
    TcpStream::connect(proxy).await.expect("first connect");

    cancel.cancel();
    // 让 accept 循环跑到 cancelled 分支并释放监听套接字
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if TcpStream::connect(proxy).await.is_err() {
            return;
        }
    }
    panic!("取消后端口仍在接受连接");
}




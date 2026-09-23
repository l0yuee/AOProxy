//! 入站认证。
//!
//! 三种入口各自对应一种校验方式：
//!
//! - 正向 HTTP 代理：`Proxy-Authorization: Basic base64(user:pass)`
//! - 正向 SOCKS5：RFC 1929 用户名/密码子协商
//! - 反向代理：URL 路径第一段作为令牌，例如 `https://host/<token>/v1/messages`
//!
//! 凭据比较一律走 [`ct_eq`]，避免因提前返回泄漏前缀长度。
//! 任何情况下都不把用户名、密码或令牌写进日志。

use base64::Engine as _;

use crate::config::{AuthConfig, AuthKind};

/// 认证结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 通过（含"本就无需认证"）。
    Ok,
    /// 缺少凭据，应回 407 / 401 要求客户端补上。
    Missing,
    /// 提供了凭据但不正确。
    Invalid,
}

impl Outcome {
    /// 是否放行。
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// 该规则是否需要对入站连接做认证。
pub fn required(cfg: &AuthConfig) -> bool {
    cfg.kind != AuthKind::None
}

/// 正向模式是否要求 Basic 认证。
pub fn basic_required(cfg: &AuthConfig) -> bool {
    cfg.kind == AuthKind::Basic
}

/// 校验 HTTP 代理的 `Proxy-Authorization` 头。
///
/// `header` 为完整头值（如 `Basic dXNlcjpwYXNz`），`None` 表示客户端未提供。
pub fn check_basic(cfg: &AuthConfig, header: Option<&str>) -> Outcome {
    if cfg.kind != AuthKind::Basic {
        return Outcome::Ok;
    }
    let Some(value) = header else {
        return Outcome::Missing;
    };
    let Some(encoded) = strip_basic_prefix(value) else {
        return Outcome::Invalid;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return Outcome::Invalid;
    };
    // 密码中可能含冒号，因此只在第一个冒号处切分
    let Some(sep) = decoded.iter().position(|b| *b == b':') else {
        return Outcome::Invalid;
    };
    let (user, pass) = (&decoded[..sep], &decoded[sep + 1..]);

    if credentials_match(cfg, user, pass) {
        Outcome::Ok
    } else {
        Outcome::Invalid
    }
}

/// 校验 SOCKS5 子协商提供的用户名与密码。
pub fn check_socks5(cfg: &AuthConfig, user: &[u8], pass: &[u8]) -> Outcome {
    if cfg.kind != AuthKind::Basic {
        return Outcome::Ok;
    }
    if credentials_match(cfg, user, pass) {
        Outcome::Ok
    } else {
        Outcome::Invalid
    }
}

/// 取 SOCKS5 服务端需要的期望凭据；`None` 表示该规则不要求认证。
pub fn socks5_credentials(cfg: &AuthConfig) -> Option<(&str, &str)> {
    if cfg.kind != AuthKind::Basic {
        return None;
    }
    match (cfg.username.as_deref(), cfg.password.as_deref()) {
        (Some(u), Some(p)) => Some((u, p)),
        _ => None,
    }
}

/// 校验反向模式的路径令牌，并返回剥掉令牌之后的路径。
///
/// - 未启用令牌：原样返回 `Ok(path)`
/// - 命中令牌：返回 `Ok(剩余路径)`，例如 `/tok/v1/messages` → `/v1/messages`
/// - 不匹配或缺失：返回 [`Outcome::Invalid`]，调用方应回 404（不提示原因，避免探测）
///
/// 剥离后若路径为空则补成 `/`，保证转发出去的请求行合法。
pub fn check_path_token(cfg: &AuthConfig, path: &str) -> std::result::Result<String, Outcome> {
    if cfg.kind != AuthKind::Token {
        return Ok(path.to_owned());
    }
    let expected = cfg.token.as_deref().unwrap_or("");
    if expected.is_empty() {
        // 校验阶段已拦截，这里防御性地拒绝
        return Err(Outcome::Invalid);
    }

    let rest = path.strip_prefix('/').unwrap_or(path);
    let (first, tail) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };

    if !ct_eq(first.as_bytes(), expected.as_bytes()) {
        return Err(Outcome::Invalid);
    }

    Ok(if tail.is_empty() {
        "/".to_owned()
    } else {
        tail.to_owned()
    })
}

// ─────────────── 内部辅助 ───────────────

fn credentials_match(cfg: &AuthConfig, user: &[u8], pass: &[u8]) -> bool {
    let expect_user = cfg.username.as_deref().unwrap_or("");
    let expect_pass = cfg.password.as_deref().unwrap_or("");
    // 两项都比较完再取与，避免用户名错误时提前返回
    let u = ct_eq(user, expect_user.as_bytes());
    let p = ct_eq(pass, expect_pass.as_bytes());
    u & p
}

/// 大小写不敏感地剥掉 `Basic ` 前缀。
fn strip_basic_prefix(value: &str) -> Option<&str> {
    let trimmed = value.trim_start();
    if trimmed.len() < 6 {
        return None;
    }
    let (scheme, rest) = trimmed.split_at(5);
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim_start())
}

/// 定长比较。长度不同直接返回 false（长度本身不算机密）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_cfg() -> AuthConfig {
        AuthConfig {
            kind: AuthKind::Basic,
            username: Some("alice".to_owned()),
            password: Some("s3cret".to_owned()),
            token: None,
        }
    }

    fn token_cfg() -> AuthConfig {
        AuthConfig {
            kind: AuthKind::Token,
            username: None,
            password: None,
            token: Some("tok123".to_owned()),
        }
    }

    fn header_for(user: &str, pass: &str) -> String {
        let raw = format!("{user}:{pass}");
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    #[test]
    fn no_auth_always_passes() {
        let cfg = AuthConfig::default();
        assert_eq!(check_basic(&cfg, None), Outcome::Ok);
        assert_eq!(check_socks5(&cfg, b"x", b"y"), Outcome::Ok);
    }

    #[test]
    fn basic_accepts_correct_credentials() {
        let cfg = basic_cfg();
        let header = header_for("alice", "s3cret");
        assert_eq!(check_basic(&cfg, Some(&header)), Outcome::Ok);
    }

    #[test]
    fn basic_rejects_wrong_credentials() {
        let cfg = basic_cfg();
        assert_eq!(check_basic(&cfg, Some(&header_for("alice", "x"))), Outcome::Invalid);
        assert_eq!(check_basic(&cfg, Some(&header_for("bob", "s3cret"))), Outcome::Invalid);
        assert_eq!(check_basic(&cfg, None), Outcome::Missing);
        assert_eq!(check_basic(&cfg, Some("Bearer abc")), Outcome::Invalid);
        assert_eq!(check_basic(&cfg, Some("Basic !!!not-base64")), Outcome::Invalid);
    }

    #[test]
    fn basic_scheme_is_case_insensitive() {
        let cfg = basic_cfg();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode("alice:s3cret");
        assert_eq!(
            check_basic(&cfg, Some(&format!("basic {encoded}"))),
            Outcome::Ok
        );
    }

    #[test]
    fn password_may_contain_colon() {
        let cfg = AuthConfig {
            kind: AuthKind::Basic,
            username: Some("alice".to_owned()),
            password: Some("a:b:c".to_owned()),
            token: None,
        };
        assert_eq!(check_basic(&cfg, Some(&header_for("alice", "a:b:c"))), Outcome::Ok);
    }

    #[test]
    fn socks5_credentials_checked() {
        let cfg = basic_cfg();
        assert_eq!(check_socks5(&cfg, b"alice", b"s3cret"), Outcome::Ok);
        assert_eq!(check_socks5(&cfg, b"alice", b"nope"), Outcome::Invalid);
        assert_eq!(socks5_credentials(&cfg), Some(("alice", "s3cret")));
        assert_eq!(socks5_credentials(&AuthConfig::default()), None);
    }

    #[test]
    fn path_token_strips_prefix() {
        let cfg = token_cfg();
        assert_eq!(check_path_token(&cfg, "/tok123/v1/messages").unwrap(), "/v1/messages");
        assert_eq!(check_path_token(&cfg, "/tok123").unwrap(), "/");
        assert_eq!(check_path_token(&cfg, "/tok123/").unwrap(), "/");
    }

    #[test]
    fn path_token_rejects_mismatch() {
        let cfg = token_cfg();
        assert_eq!(check_path_token(&cfg, "/wrong/v1"), Err(Outcome::Invalid));
        assert_eq!(check_path_token(&cfg, "/"), Err(Outcome::Invalid));
        assert_eq!(check_path_token(&cfg, "/tok123extra/v1"), Err(Outcome::Invalid));
    }

    #[test]
    fn path_untouched_without_token_auth() {
        let cfg = AuthConfig::default();
        assert_eq!(check_path_token(&cfg, "/v1/messages").unwrap(), "/v1/messages");
    }
}

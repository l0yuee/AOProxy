//! 错误类型。
//!
//! [`Display`]输出为技术性描述，用于日志与 `--log-level debug`。
//! 面向用户的提示文案由[`crate::i18n`]按[`Error::i18n_key`]取得的键查表得到。
//!
//! [`Display`]: std::fmt::Display

use std::net::SocketAddr;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    // ─────────────── 配置读写 ───────────────
    #[error("cannot determine the config directory for this platform")]
    ConfigDirUnavailable,

    #[error("failed to read config file `{path}`: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write config file `{path}`: {source}")]
    ConfigWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config file `{path}`: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize config: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("unsupported config version {found}, this build understands version {expected}")]
    ConfigVersion { found: u32, expected: u32 },

    // ─────────────── 配置校验 ───────────────
    #[error("rule `{id}`: {reason}")]
    RuleInvalid { id: String, reason: String },

    #[error("duplicate rule id `{0}`")]
    DuplicateRuleId(String),

    #[error("rules `{first}` and `{second}` both listen on {addr}")]
    ListenConflict {
        first: String,
        second: String,
        addr: String,
    },

    #[error("unknown rule `{0}`")]
    UnknownRule(String),

    // ─────────────── 运行期 ───────────────
    #[error("rule `{id}`: cannot bind {addr}: {source}")]
    Bind {
        id: String,
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("rule `{id}` is already running")]
    AlreadyRunning { id: String },

    // ─────────────── TLS ───────────────
    #[error("failed to read TLS file `{path}`: {source}")]
    TlsFileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no certificate found in `{0}`")]
    TlsNoCert(PathBuf),

    #[error("no private key found in `{0}`")]
    TlsNoKey(PathBuf),

    #[error("TLS setup failed: {0}")]
    Tls(#[from] rustls::Error),

    // ─────────────── 网络 ───────────────
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("upstream `{address}` is unreachable: {source}")]
    UpstreamUnreachable {
        address: String,
        #[source]
        source: std::io::Error,
    },

    #[error("SOCKS5 upstream error: {0}")]
    Socks(#[from] tokio_socks::Error),

    #[error("upstream proxy rejected CONNECT {target} with status {status}")]
    UpstreamConnectRejected { target: String, status: u16 },

    #[error("invalid address `{0}`")]
    InvalidAddress(String),

    // ─────────────── 开机自启 ───────────────
    #[error("cannot update the launch-at-login entry: {0}")]
    Autostart(#[source] std::io::Error),
}

impl Error {
    /// 该错误对应的 i18n 词条键，供 GUI 与 CLI 展示本地化提示。
    ///
    /// 词条中的占位符由调用方按变体自行填充；此处只负责给出键名。
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::ConfigDirUnavailable => "err.config_dir",
            Self::ConfigRead { .. } => "err.config_read",
            Self::ConfigWrite { .. } => "err.config_write",
            Self::ConfigParse { .. } => "err.config_parse",
            Self::ConfigSerialize(_) => "err.config_serialize",
            Self::ConfigVersion { .. } => "err.config_version",
            Self::RuleInvalid { .. } => "err.rule_invalid",
            Self::DuplicateRuleId(_) => "err.duplicate_id",
            Self::ListenConflict { .. } => "err.listen_conflict",
            Self::UnknownRule(_) => "err.unknown_rule",
            Self::Bind { .. } => "err.bind",
            Self::AlreadyRunning { .. } => "err.already_running",
            Self::TlsFileRead { .. } => "err.tls_file_read",
            Self::TlsNoCert(_) => "err.tls_no_cert",
            Self::TlsNoKey(_) => "err.tls_no_key",
            Self::Tls(_) => "err.tls",
            Self::Io(_) => "err.io",
            Self::UpstreamUnreachable { .. } => "err.upstream_unreachable",
            Self::Socks(_) => "err.socks",
            Self::UpstreamConnectRejected { .. } => "err.upstream_rejected",
            Self::InvalidAddress(_) => "err.invalid_address",
            Self::Autostart(_) => "err.autostart",
        }
    }

    /// 面向用户的本地化文案，按当前界面语言给出。
    ///
    /// 词条取 [`Self::i18n_key`]，占位符按变体填好；带底层原因（I/O、TLS、TOML 解析）
    /// 的变体在末尾接上原因，免得「无法绑定 127.0.0.1:8080」少了最要紧的那句
    /// 「端口已被占用」。日志与调试仍用 [`Display`] 的技术性描述。
    ///
    /// 结果总是一行：GUI 的提示条放不下多行。
    ///
    /// [`Display`]: std::fmt::Display
    pub fn localized(&self) -> String {
        let mut args: Vec<(&str, String)> = Vec::new();
        let mut cause: Option<String> = None;
        match self {
            Self::ConfigDirUnavailable => {}
            Self::ConfigRead { path, source }
            | Self::ConfigWrite { path, source }
            | Self::TlsFileRead { path, source } => {
                args.push(("path", path.display().to_string()));
                cause = Some(source.to_string());
            }
            Self::ConfigParse { path, source } => {
                args.push(("path", path.display().to_string()));
                cause = Some(toml_error_summary(source));
            }
            Self::ConfigSerialize(source) => cause = Some(source.to_string()),
            Self::ConfigVersion { found, expected } => {
                args.push(("found", found.to_string()));
                args.push(("expected", expected.to_string()));
            }
            Self::RuleInvalid { id, reason } => {
                args.push(("id", id.clone()));
                args.push(("reason", reason.clone()));
            }
            Self::DuplicateRuleId(id) | Self::UnknownRule(id) | Self::AlreadyRunning { id } => {
                args.push(("id", id.clone()));
            }
            Self::ListenConflict { first, second, addr } => {
                args.push(("first", first.clone()));
                args.push(("second", second.clone()));
                args.push(("addr", addr.clone()));
            }
            Self::Bind { id, addr, source } => {
                args.push(("id", id.clone()));
                args.push(("addr", addr.to_string()));
                cause = Some(source.to_string());
            }
            Self::TlsNoCert(path) | Self::TlsNoKey(path) => {
                args.push(("path", path.display().to_string()));
            }
            Self::Tls(source) => cause = Some(source.to_string()),
            Self::Io(source) => cause = Some(source.to_string()),
            Self::UpstreamUnreachable { address, source } => {
                args.push(("address", address.clone()));
                cause = Some(source.to_string());
            }
            Self::Socks(source) => cause = Some(source.to_string()),
            Self::UpstreamConnectRejected { target, status } => {
                args.push(("target", target.clone()));
                args.push(("status", status.to_string()));
            }
            Self::InvalidAddress(address) => args.push(("address", address.clone())),
            Self::Autostart(source) => cause = Some(source.to_string()),
        }

        let pairs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let message = crate::i18n::tr_args(self.i18n_key(), &pairs);
        match cause {
            Some(cause) => crate::i18n::tr_args(
                "err.with_cause",
                &[("message", &message), ("cause", &cause)],
            ),
            None => message,
        }
    }

    /// 构造一条规则校验错误。
    pub fn rule(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::RuleInvalid {
            id: id.into(),
            reason: reason.into(),
        }
    }
}

/// TOML 解析错误压成一行：`TOML parse error at line 3, column 1: unknown field `foo``。
///
/// toml 的 `Display` 会画出出错的那一行和指向它的箭头，占好几行，
/// 在终端里好看，塞进一行的提示条就乱了。行列号与说明才是要紧的。
fn toml_error_summary(e: &toml::de::Error) -> String {
    let rendered = e.to_string();
    let position = rendered.lines().next().unwrap_or_default();
    let message = e
        .message()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if position.is_empty() || position == message {
        message
    } else {
        format!("{position}: {message}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Language};

    /// 每个变体的占位符都得填上：留着 `{path}` 这种原样的花括号，就是漏传了参数。
    #[test]
    fn localized_fills_every_placeholder() {
        let io = || std::io::Error::new(std::io::ErrorKind::AddrInUse, "address in use");
        let path = PathBuf::from("/etc/aoproxy/config.toml");
        let parse = toml::from_str::<toml::Value>("a = ").unwrap_err();
        let errors = [
            Error::ConfigDirUnavailable,
            Error::ConfigRead { path: path.clone(), source: io() },
            Error::ConfigWrite { path: path.clone(), source: io() },
            Error::ConfigParse { path: path.clone(), source: parse },
            Error::ConfigVersion { found: 2, expected: 1 },
            Error::rule("a", "bad"),
            Error::DuplicateRuleId("a".into()),
            Error::ListenConflict { first: "a".into(), second: "b".into(), addr: "x".into() },
            Error::UnknownRule("a".into()),
            Error::Bind { id: "a".into(), addr: "127.0.0.1:1".parse().unwrap(), source: io() },
            Error::AlreadyRunning { id: "a".into() },
            Error::TlsFileRead { path: path.clone(), source: io() },
            Error::TlsNoCert(path.clone()),
            Error::TlsNoKey(path.clone()),
            Error::Io(io()),
            Error::UpstreamUnreachable { address: "h:1".into(), source: io() },
            Error::UpstreamConnectRejected { target: "h:1".into(), status: 403 },
            Error::InvalidAddress("h".into()),
            Error::Autostart(io()),
        ];
        for lang in Language::ALL {
            i18n::set_language(lang);
            for e in &errors {
                let text = e.localized();
                assert!(!text.contains('{'), "{lang}: {:?} 留下了占位符：{text}", e);
                assert!(!text.contains('\n'), "{lang}: {:?} 不止一行：{text}", e);
                assert!(!text.starts_with("err."), "{lang}: {:?} 缺词条：{text}", e);
            }
        }
        i18n::set_language(Language::default());
    }

    /// 带底层原因的错误要把原因带上：「无法绑定」而不说「端口被占用」等于没说。
    #[test]
    fn localized_keeps_the_cause() {
        let e = Error::Bind {
            id: "claude".into(),
            addr: "127.0.0.1:8080".parse().unwrap(),
            source: std::io::Error::new(std::io::ErrorKind::AddrInUse, "Address already in use"),
        };
        let text = e.localized();
        assert!(text.contains("claude"), "{text}");
        assert!(text.contains("127.0.0.1:8080"), "{text}");
        assert!(text.contains("Address already in use"), "{text}");
    }
}

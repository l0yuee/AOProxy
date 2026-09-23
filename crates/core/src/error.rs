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

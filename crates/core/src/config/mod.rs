//! 配置数据结构、读写与校验。
//!
//! 所有字段都带有 `#[serde(default)]` 或显式默认值，使得新增字段向后兼容。
//! 配置文件路径由[`config_path`]确定；写入前先校验[`Config::validate`]。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::i18n::Language;

/// 本程序能读写的配置文件版本号。升版本时递增。
pub const CONFIG_VERSION: u32 = 1;

/// 配置文件的完整内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 文件格式版本，用于前向兼容检测。
    #[serde(default = "default_version")]
    pub version: u32,

    /// 全局应用设置。
    #[serde(default)]
    pub app: AppConfig,

    /// 转发规则列表。顺序保留，用于 GUI 显示。
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

impl Config {
    /// 构造只含默认值的空配置，不含任何规则。
    pub fn default_empty() -> Self {
        Self {
            version: CONFIG_VERSION,
            app: AppConfig::default(),
            rules: Vec::new(),
        }
    }

    /// 构造示例配置，适合写到 `config init` 生成的文件中。
    pub fn example() -> Self {
        Self {
            version: CONFIG_VERSION,
            app: AppConfig::default(),
            rules: vec![
                Rule {
                    id: "claude".to_owned(),
                    name: "Claude Code".to_owned(),
                    enabled: true,
                    mode: Mode::Reverse,
                    listen: "127.0.0.1:8080".to_owned(),
                    target: Some("https://api.anthropic.com".to_owned()),
                    upstream: Upstream::default(),
                    tls: None,
                    auth: AuthConfig::default(),
                },
                Rule {
                    id: "codex".to_owned(),
                    name: "Codex".to_owned(),
                    enabled: false,
                    mode: Mode::Reverse,
                    listen: "127.0.0.1:8081".to_owned(),
                    target: Some("https://api.openai.com".to_owned()),
                    upstream: Upstream::default(),
                    tls: None,
                    auth: AuthConfig::default(),
                },
            ],
        }
    }

    /// 从文件读取并解析，自动检查版本号。
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).map_err(|e| Error::ConfigRead {
                path: path.to_owned(),
                source: e,
            })?;
        let config: Config =
            toml::from_str(&content).map_err(|e| Error::ConfigParse {
                path: path.to_owned(),
                source: e,
            })?;
        if config.version != CONFIG_VERSION {
            return Err(Error::ConfigVersion {
                found: config.version,
                expected: CONFIG_VERSION,
            });
        }
        Ok(config)
    }

    /// 序列化并写入文件，先写临时文件再原子重命名，防止写到一半崩溃导致配置损坏。
    ///
    /// 配置文件若是符号链接（dotfiles 仓库里常见），写到它指向的文件上：
    /// 直接改名会把链接本身换成一个普通文件，链接那头从此不再更新。
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)?;
        let path = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
            }
            _ => path.to_owned(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::ConfigWrite {
                path: path.clone(),
                source: e,
            })?;
        }
        let tmp = tmp_path(&path);
        write_tmp(&tmp, content.as_bytes(), &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::ConfigWrite {
                path: tmp.clone(),
                source: e,
            }
        })?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::ConfigWrite {
            path: path.clone(),
            source: e,
        })?;
        Ok(())
    }

    /// 加载配置或（文件不存在时）返回默认值，不创建文件。
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default_empty())
        }
    }

    /// 校验全部规则，返回首个错误。
    pub fn validate(&self) -> Result<()> {
        let mut seen_ids = std::collections::HashSet::new();
        let mut listen_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for rule in &self.rules {
            rule.validate()?;

            if !seen_ids.insert(rule.id.clone()) {
                return Err(Error::DuplicateRuleId(rule.id.clone()));
            }

            if let Ok(addr) = parse_listen(&rule.listen) {
                let key = addr.to_string();
                if let Some(first) = listen_map.get(&key) {
                    return Err(Error::ListenConflict {
                        first: first.clone(),
                        second: rule.id.clone(),
                        addr: key,
                    });
                }
                listen_map.insert(key, rule.id.clone());
            }
        }
        Ok(())
    }

    /// 返回启用的规则列表，保持原始顺序。
    pub fn enabled_rules(&self) -> Vec<&Rule> {
        self.rules.iter().filter(|r| r.enabled).collect()
    }

    /// 按 ID 查找规则（不区分是否启用）。
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// 按 ID 查找并可变借用规则。
    pub fn rule_mut(&mut self, id: &str) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|r| r.id == id)
    }

    /// 按 ID 覆盖同名规则，没有同名的就追加到末尾。不做校验。
    pub fn put_rule(&mut self, rule: Rule) {
        match self.rule_mut(&rule.id) {
            Some(slot) => *slot = rule,
            None => self.rules.push(rule),
        }
    }

    /// 逐条深度校验 TLS 证书与私钥是否配对，返回首个错误。
    ///
    /// 与 [`Self::validate`] 分开：此处要读盘并做密钥解析，只在 `config check`
    /// 与保存规则这类低频路径上调用。
    pub fn verify_tls_pairs(&self) -> Result<()> {
        for rule in &self.rules {
            if let Some(tls) = &rule.tls {
                tls.verify_pair().map_err(|e| Error::rule(&rule.id, e.to_string()))?;
            }
        }
        Ok(())
    }
}

/// 全局应用设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// 界面语言。不写（`None`）表示跟随系统语言，用户在设置页选过之后才记下来。
    ///
    /// 不能在第一次存盘时把当时推测出的系统语言一并写进去：那样它就成了「用户的选择」，
    /// 系统语言再变也不跟了。实际使用的语言见 [`AppConfig::effective_language`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,

    /// 日志是否开启。
    #[serde(default)]
    pub logging_enabled: bool,

    /// 日志级别。
    #[serde(default)]
    pub log_level: LogLevel,

    /// 关闭窗口时隐藏到托盘（GUI 专用）。
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: None,
            logging_enabled: false,
            log_level: LogLevel::default(),
            minimize_to_tray: true,
        }
    }
}

impl AppConfig {
    /// 实际使用的界面语言：用户选过的优先，否则跟随系统（见 [`Language::detect`]）。
    pub fn effective_language(&self) -> Language {
        self.language.unwrap_or_else(Language::detect)
    }
}

fn default_true() -> bool {
    true
}

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_filter(self) -> tracing_subscriber::filter::LevelFilter {
        use tracing_subscriber::filter::LevelFilter;
        match self {
            Self::Error => LevelFilter::ERROR,
            Self::Warn => LevelFilter::WARN,
            Self::Info => LevelFilter::INFO,
            Self::Debug => LevelFilter::DEBUG,
            Self::Trace => LevelFilter::TRACE,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warn => f.write_str("warn"),
            Self::Info => f.write_str("info"),
            Self::Debug => f.write_str("debug"),
            Self::Trace => f.write_str("trace"),
        }
    }
}

/// 一条转发规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// 规则唯一标识符，只允许字母、数字、连字符和下划线。
    pub id: String,

    /// 规则显示名称，仅用于界面与日志，可含任意文字。
    #[serde(default)]
    pub name: String,

    /// 是否启用：`aoproxy run`、「全部启用」与开机自启只启动启用的规则。
    /// 配置里不写时视为未启用——没有被明确打开过的规则不该自己跑起来。
    #[serde(default)]
    pub enabled: bool,

    /// 转发模式。
    #[serde(default)]
    pub mode: Mode,

    /// 监听地址，格式 `host:port`。
    pub listen: String,

    /// 反向模式的目标地址（正向模式可省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// 出站上游配置。
    #[serde(default)]
    pub upstream: Upstream,

    /// 入站 TLS 配置（不填则明文）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,

    /// 入站认证配置。
    #[serde(default)]
    pub auth: AuthConfig,
}

impl Rule {
    /// 返回规则的简短描述，用于日志前缀之后的 detail 字段。
    pub fn detail_str(&self) -> String {
        use crate::i18n::tr;
        let mode = tr(match self.mode {
            Mode::Forward => "mode.forward",
            Mode::Reverse => "mode.reverse",
        });
        if self.mode == Mode::Reverse {
            if let Some(target) = &self.target {
                return format!("{mode} → {target}");
            }
        }
        let upstream = match self.upstream.kind {
            UpstreamKind::Direct => tr("upstream.direct").to_owned(),
            UpstreamKind::Http => {
                format!("{} {}", tr("upstream.http"), self.upstream.address.as_deref().unwrap_or(""))
            }
            UpstreamKind::Https => {
                format!("{} {}", tr("upstream.https"), self.upstream.address.as_deref().unwrap_or(""))
            }
            UpstreamKind::Socks5 => {
                format!("{} {}", tr("upstream.socks5"), self.upstream.address.as_deref().unwrap_or(""))
            }
        };
        format!("{mode} · {upstream}")
    }

    /// 校验规则字段的合法性。
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;

        // ID 非空且只含合法字符
        if id.is_empty() {
            return Err(Error::rule(id, crate::i18n::tr("valid.id_empty")));
        }
        if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            return Err(Error::rule(id, crate::i18n::tr("valid.id_charset")));
        }

        // 监听地址可解析
        parse_listen(&self.listen).map_err(|_| {
            Error::rule(id, crate::i18n::tr("valid.listen_invalid"))
        })?;

        // 反向模式必须有目标
        if self.mode == Mode::Reverse {
            let target = self
                .target
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::rule(id, crate::i18n::tr("valid.target_required"))
                })?;
            validate_target_url(id, target)?;
        }

        // 上游
        self.upstream.validate(id)?;

        // TLS
        if let Some(tls) = &self.tls {
            tls.validate(id)?;
        }

        // 认证
        self.auth.validate(id, self.mode)?;

        Ok(())
    }
}

fn validate_target_url(rule_id: &str, target: &str) -> Result<()> {
    use crate::i18n::tr;
    // 必须以 http:// 或 https:// 开头
    if !target.starts_with("http://") && !target.starts_with("https://") {
        return Err(Error::rule(rule_id, tr("valid.target_scheme")));
    }
    // 简单的 URL 解析：去掉 scheme 后必须有非空主机名
    let without_scheme = target
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = without_scheme.split('/').next().unwrap_or("").split(':').next().unwrap_or("");
    if host.is_empty() {
        return Err(Error::rule(rule_id, tr("valid.target_host")));
    }
    Ok(())
}

/// 转发模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 正向代理：目的地由客户端请求决定。
    #[default]
    Forward,
    /// 反向代理：所有请求转发到固定目标 URL。
    Reverse,
}

/// 出站上游配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    /// 上游类型，默认直连。
    #[serde(default)]
    pub kind: UpstreamKind,

    /// 上游地址，`direct` 时为空。格式 `host:port`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,

    /// 上游认证用户名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// 上游认证密码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl Upstream {
    /// 返回 `(host, port)` 或错误。仅 `direct` 之外的类型调用。
    pub fn host_port(&self) -> Result<(&str, u16)> {
        let addr = self.address.as_deref().unwrap_or("");
        parse_host_port(addr).map_err(|_| Error::InvalidAddress(addr.to_owned()))
    }

    fn validate(&self, rule_id: &str) -> Result<()> {
        use crate::i18n::tr;
        if self.kind != UpstreamKind::Direct {
            let addr = self.address.as_deref().unwrap_or("");
            parse_host_port(addr).map_err(|_| {
                Error::rule(rule_id, tr("valid.upstream_address"))
            })?;
        }
        match (&self.username, &self.password) {
            (Some(_), None) | (None, Some(_)) => {
                return Err(Error::rule(rule_id, tr("valid.upstream_auth")));
            }
            _ => {}
        }
        Ok(())
    }
}

/// 上游代理类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamKind {
    /// 直连目标，不经过任何代理。
    #[default]
    Direct,
    /// HTTP CONNECT 代理（明文）。
    Http,
    /// HTTP CONNECT 代理（TLS）。
    Https,
    /// SOCKS5 代理。
    Socks5,
}

/// 入站 TLS 配置（可选）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM 格式证书链文件路径。
    pub cert: PathBuf,
    /// PEM 格式私钥文件路径。
    pub key: PathBuf,
}

impl TlsConfig {
    /// 深度校验：解析证书与私钥，确认二者能组成可用的 TLS 配置。
    ///
    /// [`Self::validate`] 只查文件是否存在——它在每次 [`Config::validate`] 里都会跑，
    /// 不该为此读盘解析。私钥与证书是否配对必须真正解析才知道，故单列一个方法，
    /// 由 `aoproxy config check` 与 GUI 保存规则时显式调用。
    ///
    /// 错误原样来自 TLS 层（`err.tls_no_cert` / `err.tls_no_key` / `err.tls`），
    /// 已带文件路径，调用方按需补规则上下文。
    pub fn verify_pair(&self) -> Result<()> {
        crate::engine::tls::build_acceptor(self).map(|_| ())
    }

    fn validate(&self, rule_id: &str) -> Result<()> {
        use crate::i18n::tr_args;
        if !self.cert.exists() {
            return Err(Error::rule(
                rule_id,
                tr_args(
                    "valid.tls_cert_missing",
                    &[("path", &self.cert.display().to_string())],
                ),
            ));
        }
        if !self.key.exists() {
            return Err(Error::rule(
                rule_id,
                tr_args(
                    "valid.tls_key_missing",
                    &[("path", &self.key.display().to_string())],
                ),
            ));
        }
        Ok(())
    }
}

/// 入站认证配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// 认证类型。
    #[serde(default)]
    pub kind: AuthKind,

    /// Basic 认证用户名 / SOCKS5 用户名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// Basic 认证密码 / SOCKS5 密码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// 反向模式路径令牌（置于 URL 路径第一段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl AuthConfig {
    fn validate(&self, rule_id: &str, mode: Mode) -> Result<()> {
        use crate::i18n::tr;
        match self.kind {
            AuthKind::None => {}
            AuthKind::Basic => match (&self.username, &self.password) {
                (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {}
                _ => {
                    return Err(Error::rule(
                        rule_id,
                        tr("valid.auth_basic_incomplete"),
                    ));
                }
            },
            AuthKind::Token => {
                if mode != Mode::Reverse {
                    return Err(Error::rule(
                        rule_id,
                        tr("valid.auth_token_forward"),
                    ));
                }
                if self.token.as_deref().unwrap_or("").is_empty() {
                    return Err(Error::rule(
                        rule_id,
                        tr("valid.auth_token_empty"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// 入站认证类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// 无认证。
    #[default]
    None,
    /// HTTP Basic 认证（正向/反向均可）。
    Basic,
    /// 路径令牌（仅反向模式）。
    Token,
}

// ─────────────── 路径工具 ───────────────

/// 返回平台默认的配置文件路径。
///
/// - Windows: `%APPDATA%\AOProxy\config\config.toml`
/// - Linux: `~/.config/aoproxy/config.toml`
/// - macOS: `~/Library/Application Support/AOProxy/config.toml`
pub fn config_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "AOProxy")
        .ok_or(Error::ConfigDirUnavailable)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// 位置文件名，与默认配置文件同目录。
///
/// 记录「配置文件放在别处」：GUI 设置页里换了配置文件后写这里，此后 GUI 与 CLI 都照着
/// 它找配置；删掉它就回到默认位置。这件事不能记在配置文件本身里：得先知道配置文件在哪，
/// 才读得到它。
const LOCATION_FILE: &str = "location.toml";

/// 当前生效的配置文件路径：显式给出的（命令行 `-c`）优先，其次是位置文件里记下的，
/// 最后是 [`config_path`] 给出的平台默认位置。
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let default = config_path()?;
    Ok(read_location(&default.with_file_name(LOCATION_FILE))?.unwrap_or(default))
}

/// 记下配置文件的新位置。`path` 就是默认位置时删掉位置文件，回到从没改过的状态。
pub fn set_config_location(path: &Path) -> Result<()> {
    let default = config_path()?;
    write_location(&default.with_file_name(LOCATION_FILE), path, &default)
}

/// 位置文件的内容。
#[derive(Serialize, Deserialize)]
struct Location {
    /// 配置文件的完整路径。
    config: PathBuf,
}

/// 读位置文件。文件不存在返回 `None`；存在却读不了、写坏了都报错，
/// 而不是悄悄退回默认位置——那样改的就不是用户以为的那份配置了。
fn read_location(file: &Path) -> Result<Option<PathBuf>> {
    let content = match std::fs::read_to_string(file) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::ConfigRead {
                path: file.to_owned(),
                source: e,
            })
        }
    };
    let location: Location = toml::from_str(&content).map_err(|e| Error::ConfigParse {
        path: file.to_owned(),
        source: e,
    })?;
    // 手写进去的相对路径按位置文件所在目录解析，与当前工作目录无关。
    Ok(Some(match file.parent() {
        Some(dir) if location.config.is_relative() => dir.join(location.config),
        _ => location.config,
    }))
}

fn write_location(file: &Path, path: &Path, default: &Path) -> Result<()> {
    let write_err = |e| Error::ConfigWrite {
        path: file.to_owned(),
        source: e,
    };
    if path == default {
        return match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(write_err(e)),
            _ => Ok(()),
        };
    }

    let body = toml::to_string(&Location {
        config: path.to_owned(),
    })?;
    let content = format!(
        "# AOProxy 配置文件的位置，由图形界面的设置页写入。删掉本文件即回到默认位置。\n\
         # Where AOProxy keeps its config file, set from the GUI. Delete this file to use the default location.\n\
         {body}"
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(write_err)?;
    }
    std::fs::write(file, content).map_err(write_err)
}

// ─────────────── 内部辅助 ───────────────

/// 保存用的临时文件：在原文件名后追加 `.tmp`。不用 `with_extension` 替换扩展名，
/// 否则 `a.toml` 与 `a.conf` 会撞到同一个临时文件上。
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// 写入临时文件并落盘。
///
/// 配置里有代理密码，所以权限在写入内容之前就定好：Unix 上沿用原文件的权限位
/// （用户收紧过的不能被放宽回 umask 的默认值），原文件不存在时用 0600。
/// 上次崩溃遗留的同名临时文件保留着它自己的权限，所以不能只靠创建时的 mode。
///
/// `sync_all` 之后才改名：否则断电时可能改名已生效、内容却还没写下去，留下一个空配置。
fn write_tmp(tmp: &Path, content: &[u8], original: &Path) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = std::fs::metadata(original)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(0o600);
        options.mode(mode);
        mode
    };
    #[cfg(not(unix))]
    let _ = original;

    let mut file = options.open(tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    file.write_all(content)?;
    file.sync_all()
}

fn parse_listen(s: &str) -> std::result::Result<SocketAddr, std::net::AddrParseError> {
    SocketAddr::from_str(s)
}

/// [`parse_host_port`] 的逆运算：IPv6 字面量补上方括号，`::1` + 443 → `[::1]:443`。
pub(crate) fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(crate) fn parse_host_port(s: &str) -> std::result::Result<(&str, u16), ()> {
    // "host:port" — host 可含方括号（IPv6）
    let (host, port_str) = if s.starts_with('[') {
        // IPv6 literal
        let close = s.find(']').ok_or(())?;
        let rest = &s[close + 1..];
        let port_str = rest.strip_prefix(':').ok_or(())?;
        (&s[1..close], port_str)
    } else {
        let colon = s.rfind(':').ok_or(())?;
        (&s[..colon], &s[colon + 1..])
    };
    if host.is_empty() {
        return Err(());
    }
    let port: u16 = port_str.parse().map_err(|_| ())?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_validates() {
        Config::example().validate().expect("example config should be valid");
    }

    #[test]
    fn empty_config_validates() {
        Config::default_empty().validate().expect("empty config should be valid");
    }

    #[test]
    fn duplicate_id_detected() {
        let mut c = Config::default_empty();
        c.rules.push(Rule {
            id: "a".to_owned(),
            name: "".to_owned(),
            enabled: true,
            mode: Mode::Reverse,
            listen: "127.0.0.1:9001".to_owned(),
            target: Some("https://api.anthropic.com".to_owned()),
            upstream: Upstream::default(),
            tls: None,
            auth: AuthConfig::default(),
        });
        c.rules.push(Rule {
            id: "a".to_owned(),
            name: "".to_owned(),
            enabled: true,
            mode: Mode::Reverse,
            listen: "127.0.0.1:9002".to_owned(),
            target: Some("https://api.openai.com".to_owned()),
            upstream: Upstream::default(),
            tls: None,
            auth: AuthConfig::default(),
        });
        assert!(matches!(c.validate(), Err(Error::DuplicateRuleId(_))));
    }

    #[test]
    fn listen_conflict_detected() {
        let mut c = Config::default_empty();
        for (id, port) in [("a", 9010), ("b", 9010)] {
            c.rules.push(Rule {
                id: id.to_owned(),
                name: "".to_owned(),
                enabled: true,
                mode: Mode::Reverse,
                listen: format!("127.0.0.1:{port}"),
                target: Some("https://api.anthropic.com".to_owned()),
                upstream: Upstream::default(),
                tls: None,
                auth: AuthConfig::default(),
            });
        }
        assert!(matches!(c.validate(), Err(Error::ListenConflict { .. })));
    }

    #[test]
    fn parse_host_port_ipv4() {
        assert_eq!(parse_host_port("example.com:8080"), Ok(("example.com", 8080)));
    }

    #[test]
    fn parse_host_port_ipv6() {
        assert_eq!(parse_host_port("[::1]:1234"), Ok(("::1", 1234)));
    }

    #[test]
    fn round_trip_toml() {
        let original = Config::example();
        let serialized = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(original.rules.len(), parsed.rules.len());
        assert_eq!(original.rules[0].id, parsed.rules[0].id);
    }

    // ── 辅助 ──────────────────────────────────────────────

    fn valid_reverse_rule(id: &str, port: u16) -> Rule {
        Rule {
            id: id.to_owned(),
            name: String::new(),
            enabled: true,
            mode: Mode::Reverse,
            listen: format!("127.0.0.1:{port}"),
            target: Some("https://api.anthropic.com".to_owned()),
            upstream: Upstream::default(),
            tls: None,
            auth: AuthConfig::default(),
        }
    }

    // ── Rule::validate 分支 ────────────────────────────────

    #[test]
    fn rule_id_empty_rejected() {
        let mut r = valid_reverse_rule("ok", 9100);
        r.id = String::new();
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_id_invalid_charset_rejected() {
        let mut r = valid_reverse_rule("ok", 9101);
        r.id = "bad id!".to_owned();
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_listen_invalid_rejected() {
        let mut r = valid_reverse_rule("ok", 9102);
        r.listen = "not-an-address".to_owned();
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_reverse_missing_target_rejected() {
        let mut r = valid_reverse_rule("ok", 9103);
        r.target = None;
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_target_missing_scheme_rejected() {
        let mut r = valid_reverse_rule("ok", 9104);
        r.target = Some("api.anthropic.com".to_owned());
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_target_missing_host_rejected() {
        let mut r = valid_reverse_rule("ok", 9105);
        r.target = Some("https://".to_owned());
        assert!(r.validate().is_err());
    }

    #[test]
    fn upstream_non_direct_missing_address_rejected() {
        let mut r = valid_reverse_rule("ok", 9106);
        r.upstream = Upstream {
            kind: UpstreamKind::Http,
            address: None,
            username: None,
            password: None,
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn upstream_only_username_rejected() {
        let mut r = valid_reverse_rule("ok", 9107);
        r.upstream = Upstream {
            kind: UpstreamKind::Direct,
            address: None,
            username: Some("user".to_owned()),
            password: None,
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn auth_basic_incomplete_rejected() {
        let mut r = valid_reverse_rule("ok", 9108);
        r.auth = AuthConfig {
            kind: AuthKind::Basic,
            username: Some("user".to_owned()),
            password: None,
            token: None,
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn auth_token_in_forward_mode_rejected() {
        let mut r = valid_reverse_rule("ok", 9109);
        r.mode = Mode::Forward;
        r.target = None;
        r.auth = AuthConfig {
            kind: AuthKind::Token,
            username: None,
            password: None,
            token: Some("secret".to_owned()),
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn auth_token_empty_rejected() {
        let mut r = valid_reverse_rule("ok", 9110);
        r.auth = AuthConfig {
            kind: AuthKind::Token,
            username: None,
            password: None,
            token: Some(String::new()),
        };
        assert!(r.validate().is_err());
    }

    // ── save / reload ──────────────────────────────────────

    #[test]
    fn save_and_reload_round_trip() {
        let original = Config::example();
        // 用纳秒后缀保证文件名唯一，避免并发测试冲突
        let path = std::env::temp_dir().join(format!(
            "aoproxy_test_{}.toml",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        original.save(&path).expect("save should succeed");
        let loaded = Config::load(&path).expect("load should succeed");
        let _ = std::fs::remove_file(&path);

        assert_eq!(original.rules.len(), loaded.rules.len());
        assert_eq!(original.rules[0].id,     loaded.rules[0].id);
        assert_eq!(original.rules[0].listen, loaded.rules[0].listen);
        assert_eq!(original.rules[1].id,     loaded.rules[1].id);
        assert_eq!(original.version,         loaded.version);
    }

    // ── 默认值 ─────────────────────────────────────────────

    /// 规则没写 `enabled` 时视为未启用。
    #[test]
    fn rule_enabled_defaults_to_false() {
        let cfg: Config = toml::from_str(
            "version = 1\n[[rules]]\nid = \"a\"\nmode = \"forward\"\nlisten = \"127.0.0.1:1\"\n",
        )
        .unwrap();
        assert!(!cfg.rules[0].enabled);
        assert!(cfg.enabled_rules().is_empty());
    }

    /// 没选过语言就不写 `language`，存盘也不会把推测出的系统语言固定下来；
    /// 选过的原样读写。
    #[test]
    fn language_is_only_stored_once_chosen() {
        let unset = AppConfig::default();
        assert_eq!(unset.language, None);
        let text = toml::to_string(&unset).unwrap();
        assert!(!text.contains("language"), "{text}");
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.language, None);

        let chosen = AppConfig {
            language: Some(Language::EnUs),
            ..AppConfig::default()
        };
        let back: AppConfig = toml::from_str(&toml::to_string(&chosen).unwrap()).unwrap();
        assert_eq!(back.language, Some(Language::EnUs));
        assert_eq!(back.effective_language(), Language::EnUs);
    }

    // ── 配置文件位置 ────────────────────────────────────────

    /// 每个用例一个独立的临时目录，名字带进程号与纳秒，并行跑也不会撞。
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aoproxy_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn location_round_trip_and_reset() {
        let dir = scratch_dir("location");
        let file = dir.join(LOCATION_FILE);
        let default = dir.join("config.toml");
        // 带空格与反斜杠的路径在 TOML 里要正确转义，读回来得一字不差。
        let custom = dir.join("my configs").join(r"a\b.toml");

        assert_eq!(read_location(&file).unwrap(), None, "没有位置文件时用默认位置");

        write_location(&file, &custom, &default).unwrap();
        assert_eq!(read_location(&file).unwrap(), Some(custom));

        // 换回默认位置等于删掉位置文件。
        write_location(&file, &default, &default).unwrap();
        assert!(!file.exists());
        assert_eq!(read_location(&file).unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn location_relative_path_is_relative_to_its_file() {
        let dir = scratch_dir("location_rel");
        let file = dir.join(LOCATION_FILE);
        std::fs::write(&file, "config = \"sub/aoproxy.toml\"\n").unwrap();
        assert_eq!(read_location(&file).unwrap(), Some(dir.join("sub/aoproxy.toml")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 位置文件写坏了要报出来，不能悄悄退回默认位置去改另一份配置。
    #[test]
    fn location_malformed_is_an_error() {
        let dir = scratch_dir("location_bad");
        let file = dir.join(LOCATION_FILE);
        std::fs::write(&file, "config = \n").unwrap();
        assert!(matches!(read_location(&file), Err(Error::ConfigParse { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_path_wins() {
        let explicit = Path::new("/somewhere/else.toml");
        assert_eq!(resolve_config_path(Some(explicit)).unwrap(), explicit);
    }

    /// 配置里有密码。新建的文件只给属主读写；用户手动收紧过的权限，
    /// 保存（写临时文件再改名）之后也不能被悄悄放宽回 umask 的默认值。
    #[cfg(unix)]
    #[test]
    fn save_keeps_config_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "aoproxy_perm_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let path = dir.join("config.toml");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        Config::example().save(&path).expect("首次保存");
        assert_eq!(mode(&path), 0o600, "新建的配置文件不该对其他用户可读");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        Config::example().save(&path).expect("再次保存");
        assert_eq!(mode(&path), 0o640, "保存后应当沿用文件原有的权限");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

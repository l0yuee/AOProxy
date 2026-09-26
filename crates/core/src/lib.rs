//! AOProxy 代理引擎。
//!
//! 本 crate 不依赖任何 GUI 框架，CLI 与 GUI 共用同一套引擎、配置与日志实现。
//!
//! 一条[`Rule`]描述一个完整的转发链路：入站监听 + 转发模式 + 出站通道。
//! 多条规则由[`Engine`]并行驱动，彼此独立启停，互不影响。

pub mod autostart;
pub mod config;
pub mod engine;
pub mod error;
pub mod i18n;
pub mod log;
pub mod status;

pub use config::{
    AppConfig, AuthConfig, AuthKind, Config, LogLevel, Mode, Rule, TlsConfig, Upstream, UpstreamKind,
};
pub use engine::{Engine, RuleState, RuleStatus};
pub use error::{Error, Result};
pub use i18n::Language;
pub use log::{LogBuffer, LogEntry};
pub use status::Stats;

/// 程序版本，取自 `Cargo.toml`。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 程序名，用于配置目录、日志前缀与托盘提示。
pub const APP_NAME: &str = "AOProxy";

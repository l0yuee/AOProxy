//! AOProxy 命令行界面。
//!
//! 子命令：
//!   run    — 启动代理规则并保持运行直到 Ctrl+C
//!   config — 管理配置文件（path / init / check / show）

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use aoproxy_core::{config, i18n, log as aolog, Config, Engine, Language, LogLevel};

// ─────────────── 顶层 CLI ───────────────

/// AI Agent 代理转发工具
///
/// 不带子命令时等同于 `aoproxy run`，直接按配置文件启动全部已启用的规则。
#[derive(Parser)]
#[command(
    name = "aoproxy",
    about = "AI Agent 代理转发工具",
    long_about = None,
    version,
    propagate_version = true,
    args_conflicts_with_subcommands = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// 裸调用（`aoproxy -r claude`）时的 run 参数。
    #[command(flatten)]
    run: RunArgs,
}

/// `Run` 比 `Config` 大出十来个 clap 字段，clippy 会提示 `large_enum_variant`。
/// 这里不装箱：整个进程只在 `main` 的栈上构造这一个值，随即按值交给 `run` /
/// `config_cmd`，多出的几十字节没有代价；而 clap 的 derive 不为 `Box<T>` 实现
/// `Args`，装箱要么换成手写解析，要么再加一层 newtype，都比这条提示更碍事。
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// 启动代理规则（Ctrl+C 优雅退出）
    Run(RunArgs),
    /// 管理配置文件
    Config(ConfigArgs),
}

// ─────────────── run 参数 ───────────────

#[derive(Args)]
struct RunArgs {
    /// 配置文件路径（省略时使用平台默认位置）
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// 只启动指定规则（可重复；省略时启动全部已启用规则）
    #[arg(short, long = "rule", value_name = "ID")]
    rules: Vec<String>,

    /// 日志级别 [error|warn|info|debug|trace]
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,

    /// 静默模式：不输出任何日志
    #[arg(short, long)]
    quiet: bool,

    // ── 临时规则：给出 --listen 即不读配置文件 ──
    /// 临时规则的监听地址 主机:端口（给出本项则忽略配置文件中的规则）
    #[arg(long, value_name = "ADDR")]
    listen: Option<String>,

    /// 临时规则的转发模式 [forward|reverse]
    #[arg(
        long,
        value_name = "MODE",
        default_value = "forward",
        requires = "listen"
    )]
    mode: String,

    /// 临时规则的目标地址（reverse 模式必填），如 https://api.anthropic.com
    #[arg(long, value_name = "URL", requires = "listen")]
    target: Option<String>,

    /// 临时规则的入站 Basic 认证，格式 用户名:密码
    #[arg(long, value_name = "USER:PASS", requires = "listen")]
    auth: Option<String>,

    /// 临时规则的入站认证路径令牌（仅 reverse 模式）
    #[arg(long, value_name = "TOKEN", requires = "listen")]
    auth_token: Option<String>,

    /// 临时规则的入站 TLS 证书链文件（PEM）
    #[arg(long, value_name = "FILE", requires = "listen")]
    tls_cert: Option<PathBuf>,

    /// 临时规则的入站 TLS 私钥文件（PEM）
    #[arg(long, value_name = "FILE", requires = "listen")]
    tls_key: Option<PathBuf>,

    /// 临时规则的出站上游，如 socks5://user:pass@host:1080
    #[arg(long, value_name = "URL", requires = "listen")]
    upstream: Option<String>,
}

// ─────────────── config 参数 ───────────────

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigAction,

    /// 配置文件路径（省略时使用平台默认位置）
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// 打印配置文件路径
    Path,
    /// 生成示例配置文件（文件已存在则报错）
    Init,
    /// 校验配置文件
    Check,
    /// 打印当前配置（TOML 格式）
    Show,
}

// ─────────────── 入口 ───────────────

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // 初始化 i18n（检测系统语言）
    let lang = Language::detect();
    i18n::set_language(lang);

    match cli.command {
        Some(Command::Run(args)) => run(args).await,
        Some(Command::Config(args)) => config_cmd(args),
        // 裸 `aoproxy`：按配置文件跑全部已启用规则。
        None => run(cli.run).await,
    }
}

// ─────────────── run ───────────────

async fn run(args: RunArgs) -> ExitCode {
    // 解析日志级别
    let log_level = parse_log_level(&args.log_level);
    let logging_enabled = !args.quiet;

    // 初始化日志（CLI 模式，不传 buffer）
    aolog::init(logging_enabled, log_level, None);

    // --listen 给出时走临时规则，整个配置文件不参与。
    let ad_hoc = args.listen.is_some();

    let cfg_path = if ad_hoc {
        None
    } else {
        match resolve_config_path(args.config.as_deref()) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                return ExitCode::FAILURE;
            }
        }
    };

    // 配置文件不存在就报错退出，而不是当成空配置：那样什么都没监听，
    // 进程却一直挂着，`-c` 路径写错了也看不出来。
    let config = match &cfg_path {
        Some(path) => match Config::load(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                if is_not_found(&e) {
                    eprintln!("{}", i18n::tr("cli.config_missing_hint"));
                }
                return ExitCode::FAILURE;
            }
        },
        None => match build_ad_hoc_config(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e)]));
                return ExitCode::FAILURE;
            }
        },
    };

    if let Err(e) = config.validate() {
        eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
        return ExitCode::FAILURE;
    }

    // 打印启动横幅
    if logging_enabled {
        tracing::info!(
            "{}",
            i18n::tr_args("log.banner", &[
                ("app", aoproxy_core::APP_NAME),
                ("version", aoproxy_core::VERSION),
            ])
        );
        if let Some(path) = &cfg_path {
            tracing::info!(
                "{}",
                i18n::tr_args("log.config_path", &[("path", &path.display().to_string())])
            );
        }
    }

    // 创建引擎。CLI 不改配置，故不关联配置文件路径。
    let engine = Engine::new(config);

    // 确定要启动的规则
    let errors = if args.rules.is_empty() {
        engine.start_all().await
    } else {
        let mut errs = Vec::new();
        for id in &args.rules {
            if let Err(e) = engine.start_rule(id).await {
                errs.push((id.clone(), e));
            }
        }
        errs
    };

    for (id, e) in &errors {
        tracing::error!(
            rule_id = %id,
            "{}",
            i18n::tr_args("log.rule_failed", &[("reason", &e.localized())])
        );
    }

    // 一条规则都没跑起来就退出，而不是守着一个空进程等 Ctrl+C：在 systemd 下那看起来
    // 一切正常（active），实际什么都没在监听，Restart=on-failure 也无从介入。
    let running = engine.running_count();
    if running == 0 {
        let reason = if errors.is_empty() {
            i18n::tr("log.no_rules")
        } else {
            i18n::tr("cli.all_failed")
        };
        eprintln!("{reason}");
        return ExitCode::FAILURE;
    }
    if logging_enabled {
        tracing::info!("{}", i18n::tr_args("cli.running", &[("count", &running.to_string())]));
    }

    // 等待 Ctrl+C
    wait_for_shutdown_signal().await;

    if logging_enabled {
        tracing::info!("{}", i18n::tr("log.shutdown"));
    }
    engine.stop_all().await;

    ExitCode::SUCCESS
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("无法注册 SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.expect("无法注册 Ctrl+C");
    }
}

// ─────────────── config 子命令 ───────────────

fn config_cmd(args: ConfigArgs) -> ExitCode {
    // 初始化最小日志（config 子命令不需要运行时日志）
    aolog::init(false, LogLevel::Error, None);

    let cfg_path_result = resolve_config_path(args.config.as_deref());

    match args.action {
        ConfigAction::Path => {
            match cfg_path_result {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            }
        }
        ConfigAction::Init => {
            let path = match cfg_path_result {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            };
            if path.exists() {
                eprintln!("{}", i18n::tr_args("cli.config_exists", &[("path", &path.display().to_string())]));
                eprintln!("{}", i18n::tr("cli.config_exists_hint"));
                return ExitCode::FAILURE;
            }
            let cfg = Config::example();
            match cfg.save(&path) {
                Ok(()) => println!("{}", i18n::tr_args("cli.config_written", &[("path", &path.display().to_string())])),
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            }
        }
        ConfigAction::Check => {
            let path = match cfg_path_result {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            };
            match Config::load(&path) {
                // 校验分两步：先查字段与跨规则约束，再读证书确认私钥配对。
                Ok(cfg) => match cfg.validate().and_then(|()| cfg.verify_tls_pairs()) {
                    Ok(()) => {
                        let count = cfg.rules.len();
                        println!("{}", i18n::tr_args("cli.config_ok", &[("count", &count.to_string())]));
                    }
                    Err(e) => {
                        eprintln!("{}", i18n::tr_args("cli.config_invalid", &[("reason", &e.localized())]));
                        return ExitCode::FAILURE;
                    }
                },
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            }
        }
        ConfigAction::Show => {
            let path = match cfg_path_result {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            };
            match Config::load(&path) {
                Ok(cfg) => match toml::to_string_pretty(&cfg) {
                    Ok(s) => print!("{s}"),
                    Err(e) => {
                        eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.to_string())]));
                        return ExitCode::FAILURE;
                    }
                },
                Err(e) => {
                    eprintln!("{}", i18n::tr_args("cli.error", &[("reason", &e.localized())]));
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    ExitCode::SUCCESS
}

// ─────────────── 临时规则 ───────────────

/// 由命令行参数拼出一份只含一条规则的配置，不读也不写配置文件。
///
/// 字段校验交给 [`Config::validate`]：`--mode reverse` 缺 `--target`、监听地址写错
/// 这类问题在那里统一报，错误文案与配置文件路径一致。
fn build_ad_hoc_config(args: &RunArgs) -> Result<Config, String> {
    use aoproxy_core::{AuthConfig, AuthKind, Mode, Rule};

    let listen = args.listen.clone().unwrap_or_default();

    let mode = match args.mode.to_ascii_lowercase().as_str() {
        "forward" => Mode::Forward,
        "reverse" => Mode::Reverse,
        other => return Err(format!("--mode {other}（应为 forward 或 reverse）")),
    };

    // --auth 与 --auth-token 互斥：一条规则只有一种入站认证。
    let auth = match (&args.auth, &args.auth_token) {
        (Some(_), Some(_)) => return Err("--auth 与 --auth-token 不能同时使用".to_owned()),
        (Some(pair), None) => {
            let (user, pass) = pair
                .split_once(':')
                .ok_or_else(|| "--auth 应为 用户名:密码".to_owned())?;
            AuthConfig {
                kind: AuthKind::Basic,
                username: Some(user.to_owned()),
                password: Some(pass.to_owned()),
                token: None,
            }
        }
        (None, Some(token)) => AuthConfig {
            kind: AuthKind::Token,
            username: None,
            password: None,
            token: Some(token.clone()),
        },
        (None, None) => AuthConfig::default(),
    };

    // 证书与私钥必须成对给出。
    let tls = match (&args.tls_cert, &args.tls_key) {
        (Some(cert), Some(key)) => Some(aoproxy_core::TlsConfig {
            cert: cert.clone(),
            key: key.clone(),
        }),
        (None, None) => None,
        _ => return Err(i18n::tr("valid.tls_incomplete").to_owned()),
    };

    let upstream = match &args.upstream {
        Some(url) => parse_upstream_url(url)?,
        None => aoproxy_core::Upstream::default(),
    };

    let rule = Rule {
        id: "cli".to_owned(),
        name: "CLI".to_owned(),
        enabled: true,
        mode,
        listen,
        target: args.target.clone(),
        upstream,
        tls,
        auth,
    };

    let mut config = Config::default_empty();
    config.app.logging_enabled = !args.quiet;
    config.app.log_level = parse_log_level(&args.log_level);
    config.rules.push(rule);
    Ok(config)
}

/// 解析 `scheme://[user:pass@]host:port` 形式的上游地址。
fn parse_upstream_url(url: &str) -> Result<aoproxy_core::Upstream, String> {
    use aoproxy_core::{Upstream, UpstreamKind};

    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("--upstream {url}（应为 scheme://host:port）"))?;

    let kind = match scheme.to_ascii_lowercase().as_str() {
        "direct" => return Ok(Upstream::default()),
        "http" => UpstreamKind::Http,
        "https" => UpstreamKind::Https,
        "socks5" | "socks5h" => UpstreamKind::Socks5,
        other => {
            return Err(format!(
                "--upstream 协议 {other}（应为 direct、http、https 或 socks5）"
            ))
        }
    };

    // 凭据与地址之间用最后一个 '@' 分隔：密码里可能含 '@'，主机名不会。
    let (credentials, address) = match rest.rsplit_once('@') {
        Some((cred, addr)) => (Some(cred), addr),
        None => (None, rest),
    };
    if address.is_empty() {
        return Err(format!("--upstream {url} 缺少主机与端口"));
    }

    let (username, password) = match credentials {
        Some(cred) => {
            let (u, p) = cred
                .split_once(':')
                .ok_or_else(|| "--upstream 的凭据应为 用户名:密码".to_owned())?;
            (Some(u.to_owned()), Some(p.to_owned()))
        }
        None => (None, None),
    };

    Ok(Upstream {
        kind,
        address: Some(address.to_owned()),
        username,
        password,
    })
}

// ─────────────── 工具函数 ───────────────

/// 配置文件路径：`-c` 优先，其次是 GUI 设置页记下的位置，最后是平台默认位置。
/// 与 GUI 走同一个函数，两边读写的始终是同一份配置。
fn resolve_config_path(explicit: Option<&std::path::Path>) -> aoproxy_core::Result<PathBuf> {
    config::resolve_config_path(explicit)
}

/// 是不是「配置文件不存在」：这种情况要多给一句怎么生成配置的提示。
fn is_not_found(e: &aoproxy_core::Error) -> bool {
    matches!(
        e,
        aoproxy_core::Error::ConfigRead { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound
    )
}

fn parse_log_level(s: &str) -> LogLevel {
    match s.to_ascii_lowercase().as_str() {
        "error" => LogLevel::Error,
        "warn" | "warning" => LogLevel::Warn,
        "debug" => LogLevel::Debug,
        "trace" => LogLevel::Trace,
        _ => LogLevel::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aoproxy_core::UpstreamKind;

    /// 临时规则选项不能在漏写 --listen 时被忽略，尤其是认证与 TLS 参数。
    #[test]
    fn ad_hoc_options_require_listen() {
        for prefix in [vec!["aoproxy"], vec!["aoproxy", "run"]] {
            for (option, value) in [
                ("--mode", "forward"),
                ("--target", "https://example.com"),
                ("--auth", "user:password"),
                ("--auth-token", "secret"),
                ("--tls-cert", "cert.pem"),
                ("--tls-key", "key.pem"),
                ("--upstream", "socks5://127.0.0.1:1080"),
            ] {
                let mut argv = prefix.clone();
                argv.extend([option, value]);
                let error = match Cli::try_parse_from(&argv) {
                    Err(error) => error,
                    Ok(_) => panic!("{argv:?} must require --listen"),
                };
                assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
                assert!(error.to_string().contains("--listen"));

                argv.extend(["--listen", "127.0.0.1:8080"]);
                assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
            }
        }
    }

    /// 默认 mode 不应要求 --listen；正常的配置文件调用继续生效。
    #[test]
    fn config_run_does_not_require_listen() {
        for argv in [
            vec!["aoproxy"],
            vec!["aoproxy", "run"],
            vec!["aoproxy", "-c", "config.toml", "--rule", "claude"],
            vec!["aoproxy", "run", "-c", "config.toml", "--quiet"],
            vec!["aoproxy", "config", "check", "-c", "config.toml"],
        ] {
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
    }

    // ── parse_upstream_url ─────────────────────────────────

    #[test]
    fn upstream_direct_ignores_rest() {
        let u = parse_upstream_url("direct://whatever:1080").unwrap();
        assert_eq!(u.kind, UpstreamKind::Direct);
        assert_eq!(u.address, None);
    }

    #[test]
    fn upstream_socks5_without_credentials() {
        let u = parse_upstream_url("socks5://127.0.0.1:1080").unwrap();
        assert_eq!(u.kind, UpstreamKind::Socks5);
        assert_eq!(u.address.as_deref(), Some("127.0.0.1:1080"));
        assert_eq!(u.username, None);
        assert_eq!(u.password, None);
    }

    #[test]
    fn upstream_socks5h_is_socks5() {
        assert_eq!(
            parse_upstream_url("socks5h://127.0.0.1:1080").unwrap().kind,
            UpstreamKind::Socks5
        );
    }

    #[test]
    fn upstream_scheme_is_case_insensitive() {
        assert_eq!(
            parse_upstream_url("SOCKS5://127.0.0.1:1080").unwrap().kind,
            UpstreamKind::Socks5
        );
    }

    #[test]
    fn upstream_http_and_https_kinds() {
        assert_eq!(
            parse_upstream_url("http://proxy:8080").unwrap().kind,
            UpstreamKind::Http
        );
        assert_eq!(
            parse_upstream_url("https://proxy:8443").unwrap().kind,
            UpstreamKind::Https
        );
    }

    /// 密码里的 '@' 不能把地址切错——按最后一个 '@' 分隔正是为了这个。
    #[test]
    fn upstream_password_may_contain_at_sign() {
        let u = parse_upstream_url("socks5://user:p@ss@127.0.0.1:1080").unwrap();
        assert_eq!(u.address.as_deref(), Some("127.0.0.1:1080"));
        assert_eq!(u.username.as_deref(), Some("user"));
        assert_eq!(u.password.as_deref(), Some("p@ss"));
    }

    #[test]
    fn upstream_missing_scheme_rejected() {
        assert!(parse_upstream_url("127.0.0.1:1080").is_err());
    }

    #[test]
    fn upstream_unknown_scheme_rejected() {
        assert!(parse_upstream_url("ftp://127.0.0.1:21").is_err());
    }

    #[test]
    fn upstream_empty_address_rejected() {
        assert!(parse_upstream_url("socks5://user:pass@").is_err());
        assert!(parse_upstream_url("socks5://").is_err());
    }

    #[test]
    fn upstream_credentials_without_colon_rejected() {
        assert!(parse_upstream_url("socks5://useronly@127.0.0.1:1080").is_err());
    }

    // ── parse_log_level ────────────────────────────────────

    #[test]
    fn log_level_accepts_warning_alias_and_any_case() {
        assert!(matches!(parse_log_level("warning"), LogLevel::Warn));
        assert!(matches!(parse_log_level("WARN"), LogLevel::Warn));
        assert!(matches!(parse_log_level("TRACE"), LogLevel::Trace));
    }

    /// 无法识别的级别退回 info，而不是报错退出。
    #[test]
    fn log_level_unknown_falls_back_to_info() {
        assert!(matches!(parse_log_level("verbose"), LogLevel::Info));
        assert!(matches!(parse_log_level(""), LogLevel::Info));
    }
}

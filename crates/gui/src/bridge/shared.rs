//! 进程级共享上下文。
//!
//! QML 会各自实例化 `AppBridge`、`RuleModel`、`LogModel`，但三者必须看到
//! 同一个 [`Engine`] 和同一个 [`LogBuffer`]。cxx-qt 的对象只能通过 `Default`
//! 构造，拿不到外部参数，因此在 `main` 里初始化一次，各桥接对象按需读取。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, LazyLock, OnceLock};

use parking_lot::{Mutex, RwLock};

use aoproxy_core::log::LogBuffer;
use aoproxy_core::{AppConfig, Config, Engine};

/// 引擎。配置加载失败时为 `None`，之后在设置页换上一个能读的配置文件时才建起来，
/// 所以不能像其余几项那样只设一次：桥接对象每次用都来这里取，别自己存一份。
static ENGINE: RwLock<Option<Arc<Engine>>> = RwLock::new(None);
/// 配置损坏、还没有引擎时的完整应用设置。此时仍允许改语言、日志与关闭到托盘，
/// 不能只靠各子系统的当前值重建，否则托盘选项与「跟随系统」的选择会丢失。
static FALLBACK_APP: LazyLock<RwLock<AppConfig>> =
    LazyLock::new(|| RwLock::new(AppConfig::default()));
static LOG_BUFFER: OnceLock<Arc<LogBuffer>> = OnceLock::new();
static LAUNCH: OnceLock<Launch> = OnceLock::new();
/// 日志到达通知，等 `LogModel` 构造好后被它取走。
/// 放在这里而不是 `main` 里自己起线程：刷新模型需要 `LogModel` 的
/// 线程句柄，而那个对象由 QML 实例化，`main` 拿不到。
static LOG_RX: Mutex<Option<Receiver<()>>> = Mutex::new(None);
/// 「另一个实例被启动了」的通知，等 `AppBridge` 构造好后被它取走。
/// 与 `LOG_RX` 同理：唤起窗口要用 `AppBridge` 的线程句柄，`main` 拿不到。
static SHOW_RX: Mutex<Option<Receiver<()>>> = Mutex::new(None);

/// 启动时定下来的几件事。
pub struct Launch {
    /// 这次启动用的配置文件。之后在设置页换了文件，以 `AppBridge.configPath` 为准。
    pub config_path: PathBuf,
    /// 平台默认的配置文件位置，设置页「恢复默认」用。
    pub default_config_path: PathBuf,
    /// 这次是开机自启拉起来的（命令行带了 `--autostart`）。
    pub launched_at_login: bool,
    /// 启动时读配置出的错，已是本地化文案。界面一出来就挂在提示条上，
    /// 否则用户只看到一个空空的规则列表，不知道为什么。
    pub startup_error: Option<String>,
}

/// 由 `main` 在加载 QML 之前调用一次。重复调用会被忽略。
pub fn init(
    engine: Option<Arc<Engine>>,
    buffer: Arc<LogBuffer>,
    log_rx: Receiver<()>,
    show_rx: Receiver<()>,
    launch: Launch,
) {
    if LAUNCH.set(launch).is_err() {
        return;
    }
    // main 已初始化日志。配置加载失败时为便于排错会临时打开日志，保留这一初值；
    // 语言仍是 None（跟随系统），不要把当前推测出的语言记成用户的显式选择。
    set_fallback_app(AppConfig {
        logging_enabled: aoproxy_core::log::logging_enabled(),
        log_level: aoproxy_core::log::level(),
        ..Default::default()
    });
    *ENGINE.write() = engine;
    let _ = LOG_BUFFER.set(buffer);
    *LOG_RX.lock() = Some(log_rx);
    *SHOW_RX.lock() = Some(show_rx);
}

/// 取走日志通知接收端。只有第一个调用者能拿到，之后都是 `None`——
/// QML 若重复实例化 `LogModel`，也只会有一条刷新循环。
pub fn take_log_rx() -> Option<Receiver<()>> {
    LOG_RX.lock().take()
}

/// 取走唤起窗口的通知接收端。同样只有第一个调用者拿得到。
pub fn take_show_rx() -> Option<Receiver<()>> {
    SHOW_RX.lock().take()
}

/// 共享的引擎实例。配置加载失败、还没换上能用的配置文件时为 `None`。
pub fn engine() -> Option<Arc<Engine>> {
    ENGINE.read().clone()
}

/// 装上引擎。只在启动时没有引擎、后来换上了能读的配置文件时用到。
pub fn set_engine(engine: Arc<Engine>) {
    *ENGINE.write() = Some(engine);
}

/// 当前完整配置。引擎恢复后以它为准；恢复前保留用户在设置页做的改动，
/// 也供「换用一个不存在的配置文件」把这些设置连同空规则列表一起保存。
pub fn current_config() -> Config {
    engine().map_or_else(
        || {
            let mut config = Config::default_empty();
            config.app = FALLBACK_APP.read().clone();
            config
        },
        |engine| engine.config(),
    )
}

/// 只用于还没有引擎时的设置更新。装上引擎后 `current_config` 不再读它。
pub fn set_fallback_app(app: AppConfig) {
    *FALLBACK_APP.write() = app;
}

/// 共享的日志环形缓冲。
pub fn log_buffer() -> Option<Arc<LogBuffer>> {
    LOG_BUFFER.get().cloned()
}

/// 启动信息。`init` 之前调用（不该发生）时返回 `None`。
pub fn launch() -> Option<&'static Launch> {
    LAUNCH.get()
}

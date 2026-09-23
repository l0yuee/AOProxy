//! 进程级共享上下文。
//!
//! QML 会各自实例化 `AppBridge`、`RuleModel`、`LogModel`，但三者必须看到
//! 同一个 [`Engine`] 和同一个 [`LogBuffer`]。cxx-qt 的对象只能通过 `Default`
//! 构造，拿不到外部参数，因此在 `main` 里初始化一次，各桥接对象按需读取。

use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use aoproxy_core::log::LogBuffer;
use aoproxy_core::Engine;

static ENGINE: OnceLock<Option<Arc<Engine>>> = OnceLock::new();
static LOG_BUFFER: OnceLock<Arc<LogBuffer>> = OnceLock::new();
static CONFIG_PATH: OnceLock<String> = OnceLock::new();
/// 日志到达通知，等 `LogModel` 构造好后被它取走。
/// 放在这里而不是 `main` 里自己起线程：刷新模型需要 `LogModel` 的
/// 线程句柄，而那个对象由 QML 实例化，`main` 拿不到。
static LOG_RX: Mutex<Option<Receiver<()>>> = Mutex::new(None);
/// 「另一个实例被启动了」的通知，等 `AppBridge` 构造好后被它取走。
/// 与 `LOG_RX` 同理：唤起窗口要用 `AppBridge` 的线程句柄，`main` 拿不到。
static SHOW_RX: Mutex<Option<Receiver<()>>> = Mutex::new(None);

/// 由 `main` 在加载 QML 之前调用一次。重复调用会被忽略。
pub fn init(
    engine: Option<Arc<Engine>>,
    buffer: Arc<LogBuffer>,
    log_rx: Receiver<()>,
    show_rx: Receiver<()>,
    config_path: String,
) {
    let _ = ENGINE.set(engine);
    let _ = LOG_BUFFER.set(buffer);
    *LOG_RX.lock() = Some(log_rx);
    *SHOW_RX.lock() = Some(show_rx);
    let _ = CONFIG_PATH.set(config_path);
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

/// 共享的引擎实例。配置加载失败时为 `None`。
pub fn engine() -> Option<Arc<Engine>> {
    ENGINE.get().cloned().flatten()
}

/// 共享的日志环形缓冲。
pub fn log_buffer() -> Option<Arc<LogBuffer>> {
    LOG_BUFFER.get().cloned()
}

/// 配置文件路径，用于顶栏与状态栏显示。
pub fn config_path() -> String {
    CONFIG_PATH.get().cloned().unwrap_or_default()
}

//! 日志初始化与运行期控制。
//!
//! - CLI：格式化输出到 stderr，无颜色时降级为纯文本。
//! - GUI：同时写入[`LogBuffer`]环形缓冲，供日志面板消费。
//!
//! 运行期可通过[`set_logging_enabled`]和[`set_level`]即时切换，不需要重启进程。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tracing::Level;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::LocalTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::config::LogLevel;

// ─────────────── 全局状态 ───────────────

static ENABLED: AtomicBool = AtomicBool::new(false);
/// 编码与 [`LogLevel`] 序数相同：0=Error 1=Warn 2=Info 3=Debug 4=Trace
static LEVEL: AtomicU8 = AtomicU8::new(2); // 默认 Info

/// 是否开启日志。
pub fn logging_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 运行期启用/禁用日志。
/// 禁用时 tracing 宏仍然有效，但全局过滤层会把它们全部屏蔽掉。
pub fn set_logging_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    sync_filter();
}

/// 运行期调整日志级别，无论当前是否启用。下次启用时生效。
pub fn set_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
    sync_filter();
}

fn current_level() -> LogLevel {
    match LEVEL.load(Ordering::Relaxed) {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        3 => LogLevel::Debug,
        4 => LogLevel::Trace,
        _ => LogLevel::Info,
    }
}

fn sync_filter() {
    if let Some(handle) = RELOAD_HANDLE.get() {
        let filter = runtime_filter();
        let _ = handle.reload(filter);
    }
}

fn runtime_filter() -> LevelFilter {
    if ENABLED.load(Ordering::Relaxed) {
        current_level().as_filter()
    } else {
        LevelFilter::OFF
    }
}

// ─────────────── 初始化 ───────────────

type ReloadHandle =
    tracing_subscriber::reload::Handle<LevelFilter, tracing_subscriber::Registry>;

static RELOAD_HANDLE: OnceLock<ReloadHandle> = OnceLock::new();

/// 初始化日志系统，只应在进程启动时调用一次。
///
/// - `enabled`、`level`：来自配置文件的初始值
/// - `buffer`：GUI 专用；`None` 表示 CLI 模式，只输出到 stderr
pub fn init(enabled: bool, level: LogLevel, buffer: Option<Arc<LogBuffer>>) {
    ENABLED.store(enabled, Ordering::Relaxed);
    LEVEL.store(level as u8, Ordering::Relaxed);

    let initial_filter = runtime_filter();
    let (filter_layer, reload_handle) =
        tracing_subscriber::reload::Layer::new(initial_filter);

    RELOAD_HANDLE
        .set(reload_handle)
        .expect("log::init called more than once");

    // 只有 stderr 真是终端才上色：重定向到文件时（`aoproxy 2> run.log`）
    // 转义序列会原样写进去，用编辑器打开满屏 `ESC[32m`。
    let colored = std::io::IsTerminal::is_terminal(&std::io::stderr());

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(colored)
        .event_format(CliFormat {
            timer: LocalTime::new(
                time::format_description::parse_borrowed::<2>("[hour]:[minute]:[second]").unwrap(),
            ),
        });

    if let Some(buf) = buffer {
        // GUI 模式：同时写 stderr 和环形缓冲
        let buffer_layer = GuiLayer { buffer: buf };
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer.with_filter(LevelFilter::TRACE))
            .with(buffer_layer.with_filter(LevelFilter::TRACE))
            .init();
    } else {
        // CLI 模式：只写 stderr
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .init();
    }
}

// ─────────────── CLI 行格式 ───────────────

/// 一行日志渲染成 `HH:MM:SS 级别 [规则] 正文`。
///
/// 自带的 `.compact()` 把字段排在正文之后，`rule_id` 于是落到行尾变成
/// `监听 0.0.0.0:8443 · 正向 rule_id=gateway`——读日志时最先要定位的是哪条规则，
/// 它该在最前面。字段本身不能不发：GUI 按
/// `rule_id` 取值单独成列（见 `gui/src/bridge/log_model.rs`），所以改的是渲染，
/// 不是记录方式。
struct CliFormat<T> {
    timer: T,
}

impl<S, N, T> tracing_subscriber::fmt::FormatEvent<S, N> for CliFormat<T>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
    T: tracing_subscriber::fmt::time::FormatTime,
{
    fn format_event(
        &self,
        _ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        self.timer.format_time(&mut writer)?;

        // 级别宽度固定为 5（"INFO " / "ERROR"），多行日志纵向对齐。
        let level = event.metadata().level();
        if writer.has_ansi_escapes() {
            write!(writer, " \x1b[{}m{:<5}\x1b[0m ", level_color(level), level.as_str())?;
        } else {
            write!(writer, " {:<5} ", level.as_str())?;
        }

        // 与 GUI 共用同一个访问器，两边看到的正文完全一致。
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        if let Some(id) = &visitor.rule_id {
            write!(writer, "[{id}] ")?;
        }
        writeln!(writer, "{}", visitor.message)
    }
}

/// SGR 前景色代号。终端不支持转义时由调用方走无色分支。
fn level_color(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "31", // 红
        Level::WARN => "33",  // 黄
        Level::INFO => "32",  // 绿
        Level::DEBUG => "36", // 青
        Level::TRACE => "35", // 紫
    }
}

// ─────────────── 环形缓冲（GUI 专用） ───────────────

/// 日志级别（不依赖 tracing 的类型，可自由跨线程克隆）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl EntryLevel {
    fn from_tracing(level: &Level) -> Self {
        match *level {
            Level::ERROR => Self::Error,
            Level::WARN => Self::Warn,
            Level::INFO => Self::Info,
            Level::DEBUG => Self::Debug,
            Level::TRACE => Self::Trace,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

/// 单条日志记录。
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// 日志级别。
    pub level: EntryLevel,
    /// 规则 ID（若该条日志来自某条规则）。
    pub rule_id: Option<String>,
    /// 日志正文，已完成 i18n 格式化。
    pub message: String,
    /// 本地时间字符串，格式 `HH:MM:SS`。
    pub timestamp: String,
}

/// 固定容量的环形日志缓冲，容量由`MAX`常量决定。
/// 满时丢弃最旧的记录。线程安全，GUI 可以在主线程之外写入。
#[derive(Debug)]
pub struct LogBuffer {
    inner: Mutex<LogBufferInner>,
    /// 有新记录时通过 channel 通知 GUI。
    sender: std::sync::mpsc::SyncSender<()>,
}

const LOG_BUFFER_CAPACITY: usize = 2000;

#[derive(Debug)]
struct LogBufferInner {
    entries: std::collections::VecDeque<LogEntry>,
}

impl LogBuffer {
    /// 创建新缓冲，返回缓冲本身和通知接收端（供 GUI 监听）。
    pub fn new() -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let buf = Arc::new(Self {
            inner: Mutex::new(LogBufferInner {
                entries: std::collections::VecDeque::with_capacity(LOG_BUFFER_CAPACITY),
            }),
            sender: tx,
        });
        (buf, rx)
    }

    /// 追加一条记录。
    pub fn push(&self, entry: LogEntry) {
        let mut inner = self.inner.lock();
        if inner.entries.len() >= LOG_BUFFER_CAPACITY {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
        // 忽略发送错误（接收端已关闭也无妨）
        let _ = self.sender.try_send(());
    }

    /// 快照当前所有记录（按时序最旧到最新）。
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.inner.lock().entries.iter().cloned().collect()
    }

    /// 清空缓冲。
    pub fn clear(&self) {
        self.inner.lock().entries.clear();
    }

    /// 当前记录数。
    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ─────────────── GUI Layer ───────────────

struct GuiLayer {
    buffer: Arc<LogBuffer>,
}

impl<S> tracing_subscriber::Layer<S> for GuiLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = EntryLevel::from_tracing(event.metadata().level());

        // 从事件字段提取 message 和可选的 rule_id
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let timestamp = {
            let now = time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
            format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
        };

        self.buffer.push(LogEntry {
            level,
            rule_id: visitor.rule_id,
            message: visitor.message,
            timestamp,
        });
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    rule_id: Option<String>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "rule_id" => self.rule_id = Some(value.to_owned()),
            _ => {
                if !self.message.is_empty() {
                    self.message.push(' ');
                }
                self.message.push_str(value);
            }
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}

// ─────────────── 宏辅助 ───────────────

/// 属于某条规则的日志，用于引擎内部。
///
/// ```ignore
/// log_rule!(info, rule_id, "{msg}");
/// log_rule!(warn, rule_id, "连接数 {}", count);
/// ```
///
/// 规则 ID 只作为 `rule_id` 字段发出，正文里不再重复：CLI 的行格式与 GUI 的
/// 日志面板都会把这个字段单独渲染成 `[id]`，正文里再带一份就成了 `[id] [id] …`。
#[macro_export]
macro_rules! log_rule {
    ($level:ident, $rule_id:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {
        ::tracing::$level!(rule_id = $rule_id, $fmt $(, $arg)*)
    };
}

// ─────────────── 单元测试 ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(level: EntryLevel, msg: &str) -> LogEntry {
        LogEntry {
            level,
            rule_id: None,
            message: msg.to_owned(),
            timestamp: "00:00:00".to_owned(),
        }
    }

    #[test]
    fn new_buffer_is_empty() {
        let (buf, _rx) = LogBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.snapshot().is_empty());
    }

    #[test]
    fn push_and_snapshot_preserves_order() {
        let (buf, _rx) = LogBuffer::new();
        buf.push(make_entry(EntryLevel::Info, "first"));
        buf.push(make_entry(EntryLevel::Warn, "second"));
        buf.push(make_entry(EntryLevel::Error, "third"));

        let snap = buf.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].message, "first");
        assert_eq!(snap[1].message, "second");
        assert_eq!(snap[2].message, "third");
    }

    #[test]
    fn overflow_drops_oldest_entries() {
        let (buf, _rx) = LogBuffer::new();
        // 填满再多加一条
        for i in 0..=LOG_BUFFER_CAPACITY {
            buf.push(make_entry(EntryLevel::Debug, &i.to_string()));
        }
        assert_eq!(buf.len(), LOG_BUFFER_CAPACITY);
        // 最旧的 "0" 应当被淘汰，最新的是 "2000"
        let snap = buf.snapshot();
        assert_eq!(snap[0].message, "1");
        assert_eq!(snap[LOG_BUFFER_CAPACITY - 1].message, LOG_BUFFER_CAPACITY.to_string());
    }

    #[test]
    fn clear_empties_buffer() {
        let (buf, _rx) = LogBuffer::new();
        buf.push(make_entry(EntryLevel::Info, "x"));
        buf.push(make_entry(EntryLevel::Info, "y"));
        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.snapshot().is_empty());
    }

    #[test]
    fn notification_sent_on_push() {
        let (buf, rx) = LogBuffer::new();
        // 接收端尚未收到任何通知
        assert!(rx.try_recv().is_err());
        buf.push(make_entry(EntryLevel::Trace, "ping"));
        // push 后应当能收到通知
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn entry_level_labels_exact() {
        assert_eq!(EntryLevel::Error.label(), "ERROR");
        assert_eq!(EntryLevel::Warn.label(),  "WARN ");   // 尾随空格
        assert_eq!(EntryLevel::Info.label(),  "INFO ");   // 尾随空格
        assert_eq!(EntryLevel::Debug.label(), "DEBUG");
        assert_eq!(EntryLevel::Trace.label(), "TRACE");
        // 每个 label 恰好 5 个字节（定宽对齐用）
        for level in [EntryLevel::Error, EntryLevel::Warn, EntryLevel::Info,
                      EntryLevel::Debug, EntryLevel::Trace] {
            assert_eq!(level.label().len(), 5, "label 长度不为 5: {:?}", level);
        }
    }

    #[test]
    fn rule_id_preserved_in_entry() {
        let (buf, _rx) = LogBuffer::new();
        buf.push(LogEntry {
            level: EntryLevel::Info,
            rule_id: Some("my-rule".to_owned()),
            message: "hello".to_owned(),
            timestamp: "12:34:56".to_owned(),
        });
        let snap = buf.snapshot();
        assert_eq!(snap[0].rule_id.as_deref(), Some("my-rule"));
        assert_eq!(snap[0].timestamp, "12:34:56");
    }

    // ── CLI 行格式 ─────────────────────────────────────────

    /// 固定时间戳，断言才不依赖当前时钟。
    struct FixedTime;

    impl tracing_subscriber::fmt::time::FormatTime for FixedTime {
        fn format_time(
            &self,
            w: &mut tracing_subscriber::fmt::format::Writer<'_>,
        ) -> std::fmt::Result {
            w.write_str("10:23:01")
        }
    }

    /// 收集写入的字节，用来逐字符检查渲染结果。
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 用 [`CliFormat`] 渲染 `emit` 里发出的日志，返回原文。
    ///
    /// 走的是 `with_default` 这条作用域内生效的路径，不碰 [`init`] 的全局订阅者，
    /// 因此多个用例可以并行跑。
    fn render(ansi: bool, emit: impl FnOnce()) -> String {
        let sink = Sink::default();
        let layer = tracing_subscriber::fmt::layer()
            .with_ansi(ansi)
            .with_writer(sink.clone())
            .event_format(CliFormat { timer: FixedTime });
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, emit);
        // 先把字节取出来：锁守卫若留在尾表达式里，会活到 sink 之后。
        let bytes = sink.0.lock().clone();
        String::from_utf8(bytes).expect("日志必须是合法 UTF-8")
    }

    /// 行首是时间、级别、规则 ID，正文在最后。
    /// 自带的 `.compact()` 会把 `rule_id` 排到行尾，这个用例就是防止改回去。
    #[test]
    fn cli_line_puts_rule_id_before_message() {
        let out = render(false, || {
            tracing::info!(rule_id = "gateway", "监听 0.0.0.0:8443 · 正向 · TLS · 认证");
        });
        assert_eq!(out, "10:23:01 INFO  [gateway] 监听 0.0.0.0:8443 · 正向 · TLS · 认证\n");
    }

    /// 不属于任何规则的日志（启动横幅等）不带方括号。
    #[test]
    fn cli_line_without_rule_id_has_no_brackets() {
        let out = render(false, || tracing::info!("AOProxy 0.1.0"));
        assert_eq!(out, "10:23:01 INFO  AOProxy 0.1.0\n");
    }

    /// 级别列定宽 5，长短级别混排时正文仍然对齐。
    #[test]
    fn cli_level_column_is_fixed_width() {
        let warn = render(false, || tracing::warn!("x"));
        let error = render(false, || tracing::error!("x"));
        assert_eq!(warn, "10:23:01 WARN  x\n");
        assert_eq!(error, "10:23:01 ERROR x\n");
        // 正文起始列相同才算对齐
        assert_eq!(warn.find('x'), error.find('x'));
    }

    /// `log_rule!` 发的日志里规则 ID 只出现一次：由行格式渲染成 `[id]`，正文不再带一份。
    #[test]
    fn log_rule_macro_does_not_repeat_the_rule_id() {
        let out = render(false, || crate::log_rule!(info, "gateway", "监听 {}", "0.0.0.0:8443"));
        assert_eq!(out, "10:23:01 INFO  [gateway] 监听 0.0.0.0:8443\n");
    }

    /// 重定向到文件时不能掺入转义序列，否则日志文件满屏 `ESC[32m`。
    #[test]
    fn cli_colors_only_when_ansi_enabled() {
        assert!(!render(false, || tracing::info!("plain")).contains('\x1b'));
        assert!(render(true, || tracing::info!("fancy")).contains("\x1b[32m"));
    }
}

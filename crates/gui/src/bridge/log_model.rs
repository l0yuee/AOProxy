//! `LogModel` — 日志环形缓冲的 QAbstractListModel 包装。
//!
//! GUI 日志面板绑定此模型；`AppBridge` 在收到日志通知时调用 `refresh()`。

use std::pin::Pin;
use std::sync::Arc;

use cxx_qt_lib::QString;

use aoproxy_core::log::{EntryLevel, LogBuffer, LogEntry};

// ─────────────────────── Rust 状态 ───────────────────────

pub struct LogModelState {
    buffer: Option<Arc<LogBuffer>>,
    /// 上次刷新时的快照，避免每次重绘都锁 buffer。
    entries: Vec<LogEntry>,
}

impl Default for LogModelState {
    fn default() -> Self {
        Self {
            buffer: super::shared::log_buffer(),
            entries: Vec::new(),
        }
    }
}

// ─────────────────────── cxx-qt 桥接 ───────────────────────

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        /// 基类由 Qt 提供，cxx-qt-lib 0.7 未绑定，这里只声明类型名。
        type QAbstractListModel;
    }

    unsafe extern "RustQt" {
        /// 日志列表模型，继承自 QAbstractListModel。
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        type LogModel = super::LogModelState;

        /// 拉取 buffer 最新快照并通知视图。
        #[qinvokable]
        fn refresh(self: Pin<&mut LogModel>);

        /// 清空日志缓冲和模型数据。
        #[qinvokable]
        fn clear(self: Pin<&mut LogModel>);
    }

    // ── QAbstractListModel 虚函数重写 ──
    // cxx_name 必须写成 Qt 的驼峰名，否则生成的 C++ 方法名对不上基类虚函数，
    // override 会报 C3668。
    unsafe extern "RustQt" {
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &LogModel, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(self: &LogModel, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &LogModel) -> QHash_i32_QByteArray;
    }

    // ── 从基类继承的模型重置通知 ──
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut LogModel>);

        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut LogModel>);
    }

    impl cxx_qt::Constructor<()> for LogModel {}
    impl cxx_qt::Threading for LogModel {}
}

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QVariant};

/// 日志突发时的合并窗口。`refresh` 是整表重置，一条日志刷一次会让列表
/// 在高流量下疯狂重绘；攒一小会儿再刷，界面上看不出延迟。
const COALESCE: std::time::Duration = std::time::Duration::from_millis(80);

/// `Constructor<()>` 由 `Initialize` 的 blanket impl 提供，所以必须实现它。
/// 缓冲句柄已由 `Default` 取好，这里只接上"日志到达就刷新"的循环。
impl cxx_qt::Initialize for qobject::LogModel {
    fn initialize(self: Pin<&mut Self>) {
        let qt_thread = self.qt_thread();
        // 补一次首刷：界面出现之前（读配置、启动规则）已经记下的日志
        // 不会再触发通知，不主动拉一次就看不到。排队而非直接调用，
        // 是为了让它落在构造完成之后。
        qt_thread.queue(|model| model.refresh()).ok();

        let Some(rx) = super::shared::take_log_rx() else {
            return;
        };
        std::thread::spawn(move || {
            // recv 失败说明发送端随缓冲一起没了，循环该结束。
            while rx.recv().is_ok() {
                std::thread::sleep(COALESCE);
                while rx.try_recv().is_ok() {}
                if qt_thread.queue(|model| model.refresh()).is_err() {
                    break;
                }
            }
        });
    }
}

/// 角色编号，与 [`LogModel::role_names`] 中注册的名字一一对应。
mod role {
    pub const LEVEL: i32 = 0;
    pub const TIMESTAMP: i32 = 1;
    pub const RULE_ID: i32 = 2;
    pub const MESSAGE: i32 = 3;
    pub const LEVEL_COLOR: i32 = 4;
}

impl qobject::LogModel {
    fn refresh(mut self: Pin<&mut Self>) {
        let snapshot = self
            .as_ref()
            .buffer
            .as_ref()
            .map(|b| b.snapshot())
            .unwrap_or_default();

        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().entries = snapshot;
        self.as_mut().end_reset_model();
    }

    fn clear(mut self: Pin<&mut Self>) {
        if let Some(buf) = &self.as_ref().buffer {
            buf.clear();
        }
        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().entries.clear();
        self.as_mut().end_reset_model();
    }
}

// ─────────────────────── QAbstractListModel 实现 ───────────────────────

impl qobject::LogModel {
    /// 列表模型只有顶层行，子节点行数恒为 0。
    fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            return 0;
        }
        self.rust().entries.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(e) = self.rust().entries.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            role::LEVEL => QVariant::from(&QString::from(e.level.label())),
            role::TIMESTAMP => QVariant::from(&QString::from(e.timestamp.as_str())),
            role::RULE_ID => {
                QVariant::from(&QString::from(e.rule_id.as_deref().unwrap_or("")))
            }
            role::MESSAGE => QVariant::from(&QString::from(e.message.as_str())),
            role::LEVEL_COLOR => QVariant::from(&QString::from(level_color(e.level))),
            _ => QVariant::default(),
        }
    }

    /// QML 中以 `model.<名字>` 访问，须与 [`role`] 中的编号对应。
    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut names = QHash::<QHashPair_i32_QByteArray>::default();
        names.insert(role::LEVEL, QByteArray::from("display_level"));
        names.insert(role::TIMESTAMP, QByteArray::from("display_timestamp"));
        names.insert(role::RULE_ID, QByteArray::from("display_rule_id"));
        names.insert(role::MESSAGE, QByteArray::from("display_message"));
        names.insert(role::LEVEL_COLOR, QByteArray::from("display_level_color"));
        names
    }
}

/// 级别对应的前景色，供 QML `Text.color` 直接使用。
///
/// 日志行的底色在 `bg`(#18191c) 与悬停的 `raised`(#2b2d32) 之间变，
/// 所以取色以较亮的 raised 为准，保证全部不低于 WCAG AA 的 4.5:1。
/// 原来的 Debug(#6d8ea0) 与 Trace(#4a6572) 在悬停行上只有 3.95:1 和 2.23:1——
/// 低级别日志本就该不抢眼，但「不抢眼」不等于「看不见」，
/// 于是改成降饱和而非压暗：颜色退到中性灰蓝，亮度保住。
fn level_color(level: EntryLevel) -> &'static str {
    match level {
        EntryLevel::Error => "#ff8080", // raised 5.68:1
        EntryLevel::Warn => "#ffd166",  // raised 9.56:1
        EntryLevel::Info => "#9fd2e0",  // raised 8.38:1
        EntryLevel::Debug => "#a7b4bd", // raised 6.50:1
        EntryLevel::Trace => "#8f9aa3", // raised 4.80:1
    }
}

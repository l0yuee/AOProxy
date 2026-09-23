//! `RuleModel` — 规则列表的 QAbstractListModel。
//!
//! QML ListView 绑定此模型，每行展示：名称、模式、监听地址、状态、字节统计。
//! `AppBridge::status_changed` 信号触发后调用 `refresh()` 更新数据。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt_lib::QString;

use aoproxy_core::engine::forward::format_bytes;
use aoproxy_core::{Engine, RuleStatus};

// ─────────────────────── 行数据 ───────────────────────

/// 单条规则的快照，由 Engine 状态提取。
#[derive(Default, Clone)]
pub struct RuleRow {
    pub id: String,
    pub name: String,
    pub listen: String,
    pub detail: String,
    /// "stopped" / "starting" / "running" / "failed"
    pub status: String,
    /// 配置里的启用开关，与运行状态相互独立：
    /// 启用但未启动、以及禁用却仍在运行（改了开关还没落实）都是可能的。
    pub enabled: bool,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub connections: u32,
    pub errors: u32,
}

// ─────────────────────── Rust 状态 ───────────────────────

pub struct RuleModelState {
    engine: Option<Arc<Engine>>,
    rows: Vec<RuleRow>,
    /// 行数的属性镜像。`rowCount()` 是函数，QML 的绑定不会因它的返回值
    /// 变化而重算；顶栏要显示「运行中 x / 总数」，总数就得是个带变更信号的
    /// 属性，否则增删规则后那个数字会一直停在旧值上。
    count: i32,
}

impl Default for RuleModelState {
    fn default() -> Self {
        Self {
            engine: super::shared::engine(),
            rows: Vec::new(),
            count: 0,
        }
    }
}

impl RuleModelState {
    /// 按配置中的规则顺序生成行；引擎的状态表只提供 `(id, 状态, 统计)`，
    /// 且无序，所以只用来按 ID 查状态。
    fn rebuild_rows(&mut self) {
        let Some(engine) = &self.engine else {
            self.rows.clear();
            return;
        };
        let mut statuses: HashMap<String, _> = engine
            .all_statuses()
            .into_iter()
            .map(|(id, status, snapshot)| (id, (status, snapshot)))
            .collect();
        self.rows = engine
            .config()
            .rules
            .iter()
            .map(|rule| {
                let (status, snapshot) = statuses
                    .remove(&rule.id)
                    .unwrap_or_else(|| (RuleStatus::Stopped, Default::default()));
                let status_str = match status {
                    RuleStatus::Stopped => "stopped",
                    RuleStatus::Starting => "starting",
                    RuleStatus::Running => "running",
                    RuleStatus::Failed(_) => "failed",
                }
                .to_owned();
                RuleRow {
                    id: rule.id.clone(),
                    name: rule.name.clone(),
                    listen: rule.listen.clone(),
                    detail: rule.detail_str(),
                    status: status_str,
                    enabled: rule.enabled,
                    bytes_up: snapshot.bytes_up,
                    bytes_down: snapshot.bytes_down,
                    connections: snapshot.total as u32,
                    errors: snapshot.errors as u32,
                }
            })
            .collect();
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
        /// 规则列表模型，继承自 QAbstractListModel。
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        /// 当前行数。QML 侧读 `ruleModel.count`，别用 `rowCount()`：
        /// 后者是函数，绑定不会随它变化重算。
        #[qproperty(i32, count)]
        type RuleModel = super::RuleModelState;

        /// 重新从引擎拉取规则状态并通知视图。
        #[qinvokable]
        fn refresh(self: Pin<&mut RuleModel>);

        /// 返回指定行的规则 ID，供 QML 中 start/stop 调用。
        #[qinvokable]
        fn rule_id_at(self: &RuleModel, row: i32) -> QString;
    }

    // ── QAbstractListModel 虚函数重写 ──
    // cxx_name 必须写成 Qt 的驼峰名，否则生成的 C++ 方法名对不上基类虚函数，
    // override 会报 C3668。
    unsafe extern "RustQt" {
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &RuleModel, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(self: &RuleModel, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &RuleModel) -> QHash_i32_QByteArray;
    }

    // ── 从基类继承的模型重置通知 ──
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut RuleModel>);

        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut RuleModel>);
    }

    impl cxx_qt::Constructor<()> for RuleModel {}
}

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QVariant};

/// `Constructor<()>` 由 `Initialize` 的 blanket impl 提供，所以必须实现它。
/// 引擎句柄已由 `Default` 取好，行数据等 QML 首次调用 `refresh()` 时再拉。
impl cxx_qt::Initialize for qobject::RuleModel {
    fn initialize(self: Pin<&mut Self>) {}
}

/// 角色编号，与 [`RuleModel::role_names`] 中注册的名字一一对应。
mod role {
    pub const ID: i32 = 0;
    pub const NAME: i32 = 1;
    pub const LISTEN: i32 = 2;
    pub const DETAIL: i32 = 3;
    pub const STATUS: i32 = 4;
    pub const BYTES_UP: i32 = 5;
    pub const BYTES_DOWN: i32 = 6;
    pub const CONNECTIONS: i32 = 7;
    pub const ERRORS: i32 = 8;
    pub const ENABLED: i32 = 9;
}

impl qobject::RuleModel {
    fn refresh(mut self: Pin<&mut Self>) {
        // 通知视图即将重置
        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().rebuild_rows();
        self.as_mut().end_reset_model();
        // 放在重置之后：属性变更信号会触发 QML 侧的绑定重算，
        // 此时行数据必须已经是新的。
        let count = self.as_ref().rows.len() as i32;
        self.set_count(count);
    }

    fn rule_id_at(&self, row: i32) -> QString {
        self.rust()
            .rows
            .get(row as usize)
            .map(|r| QString::from(r.id.as_str()))
            .unwrap_or_default()
    }
}

// ── QAbstractListModel 实现 ──

impl qobject::RuleModel {
    /// 列表模型只有一层，顶层行数即规则数，子项行数为 0。
    fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            return 0;
        }
        self.rust().rows.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(r) = self.rust().rows.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            role::ID => QVariant::from(&QString::from(r.id.as_str())),
            role::NAME => QVariant::from(&QString::from(r.name.as_str())),
            role::LISTEN => QVariant::from(&QString::from(r.listen.as_str())),
            role::DETAIL => QVariant::from(&QString::from(r.detail.as_str())),
            role::STATUS => QVariant::from(&QString::from(r.status.as_str())),
            role::BYTES_UP => QVariant::from(&QString::from(format_bytes(r.bytes_up).as_str())),
            role::BYTES_DOWN => {
                QVariant::from(&QString::from(format_bytes(r.bytes_down).as_str()))
            }
            role::CONNECTIONS => QVariant::from(&r.connections),
            role::ERRORS => QVariant::from(&r.errors),
            role::ENABLED => QVariant::from(&r.enabled),
            _ => QVariant::default(),
        }
    }

    /// QML 里以 `model.display_id` 等名字访问，须与 [`role`] 中的编号对应。
    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut names = QHash::<QHashPair_i32_QByteArray>::default();
        names.insert(role::ID, QByteArray::from("display_id"));
        names.insert(role::NAME, QByteArray::from("display_name"));
        names.insert(role::LISTEN, QByteArray::from("display_listen"));
        names.insert(role::DETAIL, QByteArray::from("display_detail"));
        names.insert(role::STATUS, QByteArray::from("display_status"));
        names.insert(role::BYTES_UP, QByteArray::from("display_bytes_up"));
        names.insert(role::BYTES_DOWN, QByteArray::from("display_bytes_down"));
        names.insert(role::CONNECTIONS, QByteArray::from("display_connections"));
        names.insert(role::ERRORS, QByteArray::from("display_errors"));
        names.insert(role::ENABLED, QByteArray::from("display_enabled"));
        names
    }
}

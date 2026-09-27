//! 主桥接对象 `AppBridge`。
//!
//! 经 [`shared`] 取引擎，向 QML 暴露：
//! - `configPath` 属性与 `applyConfigPath(path)`：查看、更换配置文件
//! - `startRule(id)` / `stopRule(id)` 等规则操作
//! - `autostartEnabled()` / `setAutostart(on)`：开机自启
//! - `statusChanged()` 信号，规则状态变化时触发

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt_lib::{QString, QUrl};

use aoproxy_core::{autostart, config, i18n, AppConfig, Config, Engine, Error, LogLevel, RuleStatus};

use super::rule_model::status_tag;
use super::shared;

// ─────────────────────── Rust 状态 ───────────────────────

/// AppBridge 的 Rust 侧字段（cxx-qt 要求 Data struct 实现 Default）。
///
/// 这里不存引擎：启动时配置读不出来就没有引擎，要等设置页换上一个能读的配置文件
/// 才建起来，所以每次用都去 [`shared::engine`] 取。
pub struct AppBridgeState {
    /// 当前使用的配置文件。类型必须与 `#[qproperty(QString, config_path)]` 声明一致。
    config_path: QString,
    /// 平台默认的配置文件位置，设置页「恢复默认」用。
    default_config_path: QString,
    /// 运行中的规则数，驱动托盘图标两态与悬停提示。
    running_count: i32,
    /// 最近一次操作的错误文案，空串表示无错误。
    last_error: QString,
    /// 托盘是否可用。Linux 上检测不到 StatusNotifier 时为 false，
    /// 关闭窗口改为退出，避免程序藏进不存在的托盘里找不回来。
    tray_available: bool,
    /// 当前语言标签，仅用于让 QML 的 `tr()` 绑定在语言切换后失效重算。
    language: QString,
    /// 程序版本，取自 `aoproxy_core::VERSION`（即 Cargo.toml 里的版本号）。
    /// 走属性而不是让 QML 写死：版本号只该有一处来源，
    /// 界面上的和 `aoproxy --version` 报的必须是同一个。
    version: QString,
    /// 本平台能否开机自启，不能时设置页的开关置灰。
    autostart_supported: bool,
    /// 这次是开机自启拉起来的：界面缩在托盘里，并运行已启用的规则。
    launched_at_login: bool,
    /// 日志开关与级别的属性镜像。托盘菜单和设置页都能改这两项，
    /// 做成带变更信号的属性，一边改了另一边的勾选状态跟着变。
    logging_enabled: bool,
    log_level: QString,
}

impl Default for AppBridgeState {
    fn default() -> Self {
        // 引擎与启动信息在 main 中初始化一次，
        // 确保与 RuleModel / LogModel 看到的是同一个实例。
        let launch = shared::launch();
        Self {
            config_path: launch.map(|l| path_qstring(&l.config_path)).unwrap_or_default(),
            default_config_path: launch
                .map(|l| path_qstring(&l.default_config_path))
                .unwrap_or_default(),
            running_count: shared::engine().map_or(0, |e| e.running_count() as i32),
            // 启动时读配置出的错一开始就挂上：否则只看到一个空列表，不知道为什么。
            last_error: QString::from(
                launch
                    .and_then(|l| l.startup_error.as_deref())
                    .unwrap_or_default(),
            ),
            tray_available: super::tray_available(),
            language: QString::from(i18n::language().tag()),
            version: QString::from(aoproxy_core::VERSION),
            autostart_supported: autostart::is_supported(),
            launched_at_login: launch.is_some_and(|l| l.launched_at_login),
            logging_enabled: current_app().logging_enabled,
            log_level: QString::from(current_app().log_level.to_string().as_str()),
        }
    }
}

// ─────────────────────── cxx-qt 桥接 ───────────────────────

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    unsafe extern "RustQt" {
        /// 主控制桥接对象，在 QML 中以 `AppBridge` 类型注册。
        ///
        /// cxx-qt 默认按 Rust 函数名生成 Qt 侧名字，不会转驼峰。QML 习惯用
        /// 驼峰（信号 `status_changed` 的处理器会变成 `onStatus_changed`），
        /// 所以这里统一用 `cxx_name` 指定 Qt 侧名称。
        #[qobject]
        #[qml_element]
        #[qproperty(QString, config_path, cxx_name = "configPath")]
        #[qproperty(QString, default_config_path, cxx_name = "defaultConfigPath")]
        #[qproperty(i32, running_count, cxx_name = "runningCount")]
        #[qproperty(QString, last_error, cxx_name = "lastError")]
        #[qproperty(bool, tray_available, cxx_name = "trayAvailable")]
        /// 当前语言标签。QML 的 `tr()` 是函数而非属性，绑定不会自动重算；
        /// 让辅助函数在调用 `tr()` 前先读一次这个属性，语言一变绑定即失效重算。
        ///
        /// 属性自带的 `languageChanged` 通知信号就是 QML 侧接的那个，
        /// 不要再手写一个同名 `#[qsignal]`——cxx 会报重复定义。
        #[qproperty(QString, language)]
        /// 程序版本号，只读。窗口标题、顶栏徽标与设置页的「关于」行都读它。
        #[qproperty(QString, version)]
        #[qproperty(bool, autostart_supported, cxx_name = "autostartSupported")]
        #[qproperty(bool, launched_at_login, cxx_name = "launchedAtLogin")]
        /// 日志是否开启、日志级别（`error` … `trace`），只读镜像：
        /// 改用 `enableLogging` / `selectLogLevel`，直接赋值不会落盘。
        #[qproperty(bool, logging_enabled, cxx_name = "loggingEnabled")]
        #[qproperty(QString, log_level, cxx_name = "logLevel")]
        type AppBridge = super::AppBridgeState;

        /// 规则状态发生变化（启动/停止/失败）时发出，QML 监听后刷新列表。
        /// QML 侧对应 `onStatusChanged`。
        #[qsignal]
        #[cxx_name = "statusChanged"]
        fn status_changed(self: Pin<&mut AppBridge>);

        /// 换了配置文件，规则与应用设置都已是新文件里的。设置页据此重读各项。
        #[qsignal]
        #[cxx_name = "configChanged"]
        fn config_changed(self: Pin<&mut AppBridge>);

        /// 启动指定 ID 的规则。
        #[qinvokable]
        #[cxx_name = "startRule"]
        fn start_rule(self: Pin<&mut AppBridge>, id: &QString);

        /// 停止指定 ID 的规则。
        #[qinvokable]
        #[cxx_name = "stopRule"]
        fn stop_rule(self: Pin<&mut AppBridge>, id: &QString);

        /// 启动配置中全部已启用的规则。
        #[qinvokable]
        #[cxx_name = "startAll"]
        fn start_all(self: Pin<&mut AppBridge>);

        /// 停止全部规则。
        #[qinvokable]
        #[cxx_name = "stopAll"]
        fn stop_all(self: Pin<&mut AppBridge>);

        /// 切换规则的启用开关，随即启动或停止并落盘。
        #[qinvokable]
        #[cxx_name = "setRuleEnabled"]
        fn set_rule_enabled(self: Pin<&mut AppBridge>, id: &QString, enabled: bool);

        /// 保存前的完整校验，返回空串表示可以存，否则是错误文案。
        /// `is_new` 为真时 ID 不得与已有规则重复。
        #[qinvokable]
        #[cxx_name = "checkRule"]
        fn check_rule(self: &AppBridge, json: &QString, is_new: bool) -> QString;

        /// 新增规则（`is_new`）或按 ID 覆盖已有规则。`json` 为编辑对话框序列化出的
        /// 规则对象；校验失败时不写盘，错误经 `lastError` 反馈。
        #[qinvokable]
        #[cxx_name = "saveRule"]
        fn save_rule(self: Pin<&mut AppBridge>, json: &QString, is_new: bool);

        /// 删除规则。
        #[qinvokable]
        #[cxx_name = "deleteRule"]
        fn delete_rule(self: Pin<&mut AppBridge>, id: &QString);

        /// 读取单条规则的完整配置，返回 JSON 供编辑对话框填充。
        /// 规则不存在时返回按 `listen` 等字段留空的新规则模板。
        #[qinvokable]
        #[cxx_name = "ruleJson"]
        fn rule_json(self: &AppBridge, id: &QString) -> QString;

        /// 全局应用设置（语言、日志开关与级别、关闭到托盘），JSON 形式。
        #[qinvokable]
        #[cxx_name = "appConfigJson"]
        fn app_config_json(self: &AppBridge) -> QString;

        /// 替换全局应用设置，即时生效并落盘。
        #[qinvokable]
        #[cxx_name = "saveAppConfig"]
        fn save_app_config(self: Pin<&mut AppBridge>, json: &QString);

        /// 打开或关闭日志，即时生效并落盘。托盘菜单用。
        #[qinvokable]
        #[cxx_name = "enableLogging"]
        fn enable_logging(self: Pin<&mut AppBridge>, enabled: bool);

        /// 改日志级别（`error` | `warn` | `info` | `debug` | `trace`），即时生效并落盘。托盘菜单用。
        #[qinvokable]
        #[cxx_name = "selectLogLevel"]
        fn select_log_level(self: Pin<&mut AppBridge>, level: &QString);

        /// 托盘菜单里的规则列表，JSON 数组，按配置顺序，元素为
        /// `{id, name, enabled, status}`；`status` 取值同规则列表（`running` 等）。
        #[qinvokable]
        #[cxx_name = "ruleMenuJson"]
        fn rule_menu_json(self: &AppBridge) -> QString;

        /// 换用另一个配置文件：文件已存在就载入它，不存在就把当前配置存过去；
        /// 并记下这个位置，下次启动（GUI 与 CLI）照旧用它。填的就是当前文件时，
        /// 等于从磁盘重新载入一遍——在外面改过文件、或启动时它没能读出来，都用得上。
        ///
        /// 路径与文件本身的问题当场返回错误文案；返回空串表示已开始切换，
        /// 切换完成时发 `configChanged`。
        #[qinvokable]
        #[cxx_name = "applyConfigPath"]
        fn apply_config_path(self: Pin<&mut AppBridge>, path: &QString) -> QString;

        /// 文件对话框给的 URL 转成本地路径，不是本地文件时返回空串。
        #[qinvokable]
        #[cxx_name = "urlToPath"]
        fn url_to_path(self: &AppBridge, url: &QUrl) -> QString;

        /// 某个文件所在目录的 URL，给文件对话框当初始目录。
        #[qinvokable]
        #[cxx_name = "folderUrl"]
        fn folder_url(self: &AppBridge, path: &QString) -> QUrl;

        /// 当前这个程序是否已登记为开机自启。每次都去问系统，
        /// 用户在系统设置里关掉的，这里也能看出来。
        #[qinvokable]
        #[cxx_name = "autostartEnabled"]
        fn autostart_enabled(self: &AppBridge) -> bool;

        /// 登记或撤销开机自启，返回是否成功；失败原因写进 `lastError`。
        #[qinvokable]
        #[cxx_name = "setAutostart"]
        fn set_autostart(self: Pin<&mut AppBridge>, enabled: bool) -> bool;

        /// 按当前语言取词条，QML 的界面文案全部经此获取，
        /// 使语言切换无需重启即可生效。
        #[qinvokable]
        fn tr(self: &AppBridge, key: &QString) -> QString;

        /// 带占位符的词条，如 `运行中 {count} / {total}`。
        /// `args_json` 为 JSON 对象，键名对应大括号里的占位符：
        /// `trFmt("gui.running_count", JSON.stringify({count: 2, total: 5}))`。
        /// 走 JSON 是为了跟规则、设置一致——桥上只传字符串，不传结构体。
        #[qinvokable]
        #[cxx_name = "trFmt"]
        fn tr_fmt(self: &AppBridge, key: &QString, args_json: &QString) -> QString;

        /// 带占位符的词条，如 `{count}`、`{name}`。`args` 是 JSON 对象，
        /// QML 侧写 `trArgs("gui.x", JSON.stringify({count: 3}))`。
        /// 走 JSON 而不是逐个参数，是因为 cxx-qt 传不了变长参数表。
        #[qinvokable]
        #[cxx_name = "trArgs"]
        fn tr_args(self: &AppBridge, key: &QString, args: &QString) -> QString;

        /// 可选语言列表，JSON 数组，元素为 `{tag, name}`。
        #[qinvokable]
        #[cxx_name = "languageList"]
        fn language_list(self: &AppBridge) -> QString;

        /// 停止全部规则后请求退出。退出确认由 QML 侧的对话框负责。
        #[qinvokable]
        fn quit(self: Pin<&mut AppBridge>);

        /// 全部规则已停妥、可以安全退出时发出，QML 侧接 `Qt.quit()`。
        ///
        /// 退出经由信号而非 `std::process::exit`：让 Qt 自己走完事件循环的
        /// 收尾，否则 tokio 任务可能在连接写到一半时被直接掐断。
        /// （cxx-qt-lib 0.7 没有 `QCoreApplication::exit` 绑定，只能这么走。）
        #[qsignal]
        #[cxx_name = "quitRequested"]
        fn quit_requested(self: Pin<&mut AppBridge>);

        /// 又有人启动了本程序（单实例守卫拦下的那一次），QML 接住后把窗口唤到前台。
        #[qsignal]
        #[cxx_name = "showRequested"]
        fn show_requested(self: Pin<&mut AppBridge>);

        /// 清除 `lastError`。界面上的提示条关掉时调用，
        /// 免得同一条错误在下次操作成功前一直挂着。
        #[qinvokable]
        #[cxx_name = "clearError"]
        fn clear_error(self: Pin<&mut AppBridge>);
    }

    impl cxx_qt::Constructor<()> for AppBridge {}
    impl cxx_qt::Threading for AppBridge {}
}

use cxx_qt::Threading;

/// `Constructor<()>` 由 `Initialize` 的 blanket impl 提供，所以必须实现它。
///
/// 字段本身由 `Default` 从 `shared` 取好；这里只接上单实例守卫的唤起通道。
impl cxx_qt::Initialize for qobject::AppBridge {
    fn initialize(self: Pin<&mut Self>) {
        let Some(rx) = shared::take_show_rx() else {
            return;
        };
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            // recv 失败说明发送端随监听线程一起没了，循环该结束。
            while rx.recv().is_ok() {
                if qt_thread.queue(|bridge| bridge.show_requested()).is_err() {
                    break;
                }
            }
        });
    }
}

impl qobject::AppBridge {
    /// 启动规则：在 tokio runtime 上异步调用，结果通过信号通知 QML。
    fn start_rule(self: Pin<&mut Self>, id: &QString) {
        let id_str = id.to_string();
        self.spawn_rule_op(move |engine| async move { engine.start_rule(&id_str).await });
    }

    /// 停止规则：在 tokio runtime 上异步调用。
    fn stop_rule(self: Pin<&mut Self>, id: &QString) {
        let id_str = id.to_string();
        self.spawn_rule_op(move |engine| async move {
            engine.stop_rule(&id_str).await
        });
    }

    /// 启动全部已启用规则。逐条失败都记在日志里，这里只反馈第一条错误，
    /// 避免把一串规则 ID 堆到界面的提示条上。
    fn start_all(self: Pin<&mut Self>) {
        self.spawn_rule_op(|engine| async move {
            match engine.start_all().await.into_iter().next() {
                Some((_, e)) => Err(e),
                None => Ok(()),
            }
        });
    }

    fn stop_all(self: Pin<&mut Self>) {
        self.spawn_rule_op(|engine| async move {
            engine.stop_all().await;
            Ok(())
        });
    }

    fn set_rule_enabled(self: Pin<&mut Self>, id: &QString, enabled: bool) {
        let id_str = id.to_string();
        self.spawn_rule_op(move |engine| async move {
            engine.set_rule_enabled(&id_str, enabled).await
        });
    }

    fn check_rule(&self, json: &QString, is_new: bool) -> QString {
        QString::from(rule_problem(&json.to_string(), is_new).unwrap_or_default().as_str())
    }

    /// 保存规则。JSON 解析失败与配置校验失败走同一条反馈路径：
    /// 都是"这份表单不能存"，界面只需要一句话说明原因。
    fn save_rule(self: Pin<&mut Self>, json: &QString, is_new: bool) {
        let text = json.to_string();
        let rule = match serde_json::from_str::<aoproxy_core::Rule>(&text) {
            Ok(r) => r,
            Err(e) => {
                self.report_text(form_error(&e));
                return;
            }
        };
        self.spawn_rule_op(move |engine| async move {
            // 新建的规则撞上已有 ID 时拒绝：按 ID 覆盖会把那条规则悄无声息地换掉。
            // 编辑对话框保存前已用 checkRule 查过，这里防的是两次操作之间的空档。
            if is_new && engine.config().rule(&rule.id).is_some() {
                return Err(Error::DuplicateRuleId(rule.id));
            }
            engine.upsert_rule(rule).await
        });
    }

    fn delete_rule(self: Pin<&mut Self>, id: &QString) {
        let id_str = id.to_string();
        self.spawn_rule_op(move |engine| async move { engine.remove_rule(&id_str).await });
    }

    /// 取单条规则的 JSON。ID 为空或查不到时返回新建用的模板，
    /// 这样编辑对话框只有一条填充路径，不必区分新建与修改。
    fn rule_json(&self, id: &QString) -> QString {
        let id_str = id.to_string();
        let rule = shared::engine()
            .and_then(|e| e.config().rule(&id_str).cloned())
            .unwrap_or_else(default_rule);
        QString::from(serde_json::to_string(&rule).unwrap_or_default().as_str())
    }

    fn app_config_json(&self) -> QString {
        QString::from(serde_json::to_string(&current_app()).unwrap_or_default().as_str())
    }

    /// 应用全局设置（设置页整份发来）。
    fn save_app_config(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<AppConfig>(&json.to_string()) {
            Ok(app) => self.apply_app(app),
            Err(e) => self.report_text(form_error(&e)),
        }
    }

    fn enable_logging(self: Pin<&mut Self>, enabled: bool) {
        let app = AppConfig {
            logging_enabled: enabled,
            ..current_app()
        };
        self.apply_app(app);
    }

    fn select_log_level(self: Pin<&mut Self>, level: &QString) {
        // 级别名与配置文件里的写法相同，按同一套反序列化规则解析。
        let level = serde_json::Value::String(level.to_string());
        match serde_json::from_value::<LogLevel>(level) {
            Ok(log_level) => {
                let app = AppConfig {
                    log_level,
                    ..current_app()
                };
                self.apply_app(app);
            }
            Err(e) => self.report_text(form_error(&e)),
        }
    }

    fn rule_menu_json(&self) -> QString {
        let items: Vec<_> = shared::engine()
            .map(|engine| {
                engine
                    .config()
                    .rules
                    .iter()
                    .map(|rule| {
                        let status = engine.rule_status(&rule.id).unwrap_or(RuleStatus::Stopped);
                        serde_json::json!({
                            "id": rule.id,
                            "name": rule.name,
                            "enabled": rule.enabled,
                            "status": status_tag(&status),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        QString::from(serde_json::to_string(&items).unwrap_or_default().as_str())
    }

    fn apply_config_path(self: Pin<&mut Self>, path: &QString) -> QString {
        let text = path.to_string();
        let path = PathBuf::from(text.trim());
        if path.as_os_str().is_empty() {
            return QString::from(i18n::tr("gui.config_path_empty"));
        }
        // 相对路径相对的是进程的工作目录，而 GUI 的工作目录取决于从哪儿启动的，
        // 同一个写法两次启动可能指向两个文件。
        if !path.is_absolute() {
            return QString::from(i18n::tr("gui.config_path_relative"));
        }

        let prepared = prepare_config(&path).and_then(|config| {
            config::set_config_location(&path)?;
            Ok(config)
        });
        let config = match prepared {
            Ok(config) => config,
            Err(e) => return QString::from(e.localized().as_str()),
        };

        let qt_thread = self.qt_thread();
        tokio::spawn(async move {
            match shared::engine() {
                Some(engine) => engine.switch_config(config, path.clone()).await,
                // 启动时配置没读出来，至今还没有引擎：用新配置建一个。
                None => {
                    let engine = Arc::new(Engine::new(Config::default_empty()));
                    engine.switch_config(config, path.clone()).await;
                    shared::set_engine(engine);
                }
            }
            qt_thread
                .queue(move |mut bridge| {
                    bridge.as_mut().set_config_path(path_qstring(&path));
                    bridge.as_mut().report_text(String::new());
                    bridge.as_mut().sync_app_state();
                    bridge.as_mut().refresh_running_count();
                    bridge.as_mut().config_changed();
                    bridge.status_changed();
                })
                .ok();
        });
        QString::default()
    }

    fn url_to_path(&self, url: &QUrl) -> QString {
        // Qt 给的是正斜杠；换成本平台的写法，设置页上显示的才是用户熟悉的样子。
        url.to_local_file()
            .map(|p| path_qstring(&PathBuf::from(p.to_string())))
            .unwrap_or_default()
    }

    fn folder_url(&self, path: &QString) -> QUrl {
        let path = PathBuf::from(path.to_string());
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(&path);
        QUrl::from_local_file(&path_qstring(dir))
    }

    fn autostart_enabled(&self) -> bool {
        autostart::is_enabled().unwrap_or(false)
    }

    fn set_autostart(self: Pin<&mut Self>, enabled: bool) -> bool {
        match autostart::set_enabled(enabled) {
            Ok(()) => true,
            Err(e) => {
                self.report_text(e.localized());
                false
            }
        }
    }

    fn tr(&self, key: &QString) -> QString {
        QString::from(i18n::tr(&key.to_string()))
    }

    /// 占位符替换。参数值按 JSON 原样取字符串：数字用 `to_string`，
    /// 字符串去掉引号，其余（数组、对象）用紧凑 JSON——占位符里塞复杂结构
    /// 属于用错了，但也不至于让整条文案变空。
    fn tr_fmt(&self, key: &QString, args_json: &QString) -> QString {
        let parsed: serde_json::Value =
            serde_json::from_str(&args_json.to_string()).unwrap_or(serde_json::Value::Null);
        let owned: Vec<(String, String)> = match parsed {
            serde_json::Value::Object(map) => map
                .into_iter()
                .map(|(k, v)| {
                    let text = match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, text)
                })
                .collect(),
            _ => Vec::new(),
        };
        let args: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        QString::from(i18n::tr_args(&key.to_string(), &args).as_str())
    }

    /// 占位符全部按字符串替换，数字由 QML 侧 `JSON.stringify` 自然转成
    /// 数字字面量，这里统一按文本取。解析不了就退回无参词条，
    /// 让界面上出现原文而不是空白。
    fn tr_args(&self, key: &QString, args: &QString) -> QString {
        let key = key.to_string();
        let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            &args.to_string(),
        ) else {
            return QString::from(i18n::tr(&key));
        };
        let owned: Vec<(String, String)> = map
            .into_iter()
            .map(|(k, v)| {
                let text = match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                (k, text)
            })
            .collect();
        let pairs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        QString::from(i18n::tr_args(&key, &pairs).as_str())
    }

    /// 语言下拉框的数据源。`native_name` 用各语言自己的写法，
    /// 用户在看不懂当前界面语言时也能找到自己的那一项。
    fn language_list(&self) -> QString {
        let items: Vec<_> = aoproxy_core::Language::ALL
            .iter()
            .map(|lang| {
                serde_json::json!({ "tag": lang.tag(), "name": lang.native_name() })
            })
            .collect();
        QString::from(serde_json::to_string(&items).unwrap_or_default().as_str())
    }

    fn clear_error(self: Pin<&mut Self>) {
        self.report_text(String::new());
    }

    /// 退出：先停全部规则，等监听端口干净释放后再让 QML 关掉应用。
    fn quit(self: Pin<&mut Self>) {
        let qt_thread = self.qt_thread();
        tokio::spawn(async move {
            if let Some(engine) = shared::engine() {
                engine.stop_all().await;
            }
            qt_thread
                .queue(|mut bridge| {
                    bridge.as_mut().refresh_running_count();
                    bridge.quit_requested();
                })
                .ok();
        });
    }
}

// ─────────────────────── 内部辅助 ───────────────────────

impl qobject::AppBridge {
    /// 在 tokio 上跑一个返回 `Result` 的规则操作，完成后回 Qt 线程
    /// 刷新运行计数、反馈错误并通知界面。
    ///
    /// 所有改动状态的入口都走这里，保证「操作 → 反馈 → 刷新」三步齐全：
    /// 漏掉刷新会让界面停在旧状态，漏掉反馈会让失败静静消失。
    fn spawn_rule_op<F, Fut>(self: Pin<&mut Self>, op: F)
    where
        F: FnOnce(Arc<Engine>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = aoproxy_core::Result<()>> + Send,
    {
        let Some(engine) = shared::engine() else {
            // 配置没读出来，没有引擎可操作。说一声，免得点了没反应。
            self.report_text(i18n::tr("gui.no_config").to_owned());
            return;
        };
        let qt_thread = self.qt_thread();
        tokio::spawn(async move {
            let result = op(engine).await;
            qt_thread
                .queue(move |mut bridge| {
                    bridge.as_mut().report(result);
                    bridge.as_mut().refresh_running_count();
                    bridge.status_changed();
                })
                .ok();
        });
    }

    /// 把操作结果写进 `lastError`：成功清空，失败填 i18n 文案。
    /// 每次都写是有意的——不清空的话，上一次的失败会一直挂在界面上。
    fn report(self: Pin<&mut Self>, result: aoproxy_core::Result<()>) {
        let text = match result {
            Ok(()) => String::new(),
            Err(e) => e.localized(),
        };
        self.report_text(text);
    }

    /// 直接写入一条错误文案。空串表示"没有错误"，界面据此收起提示条。
    fn report_text(self: Pin<&mut Self>, text: String) {
        self.set_last_error(QString::from(text.as_str()));
    }

    fn refresh_running_count(self: Pin<&mut Self>) {
        let count = shared::engine().map_or(0, |e| e.running_count() as i32);
        self.set_running_count(count);
    }

    /// 应用全局设置：语言、日志开关与级别由核心即时生效并落盘，随后同步各属性。
    ///
    /// 设置页与托盘菜单都走这里：两边改的是同一份设置，属性一变另一边就跟着变。
    fn apply_app(mut self: Pin<&mut Self>, app: AppConfig) {
        let result = match shared::engine() {
            Some(engine) => engine.update_app_config(app),
            // 配置没读出来，存不了盘；至少让语言与日志开关当场生效。
            None => {
                i18n::set_language(app.effective_language());
                aoproxy_core::log::set_logging_enabled(app.logging_enabled);
                aoproxy_core::log::set_level(app.log_level);
                Ok(())
            }
        };
        self.as_mut().report(result);
        self.as_mut().sync_app_state();
        self.status_changed();
    }

    /// 应用设置可能刚变过（改了设置、换了配置文件）：把属性镜像对齐到当前值。
    ///
    /// 语言那一项总是重发 `languageChanged`：属性变化会让 QML 里所有经辅助函数取的
    /// 文案重算，信号则供需要手动刷新的地方（如托盘菜单）使用。日志两项由属性的
    /// setter 在值真的变了时才发信号。
    fn sync_app_state(mut self: Pin<&mut Self>) {
        let app = current_app();
        self.as_mut().set_logging_enabled(app.logging_enabled);
        self.as_mut()
            .set_log_level(QString::from(app.log_level.to_string().as_str()));
        let tag = QString::from(i18n::language().tag());
        self.as_mut().set_language(tag);
        self.language_changed();
    }
}

/// 当前的应用设置。没有引擎（配置没读出来）时存不了盘，就取本进程正在用的语言与
/// 日志设置：否则托盘里刚打开的日志一读回来又成了关、刚选的语言又被换回系统语言。
fn current_app() -> AppConfig {
    shared::engine().map_or_else(
        || AppConfig {
            language: Some(i18n::language()),
            logging_enabled: aoproxy_core::log::logging_enabled(),
            log_level: aoproxy_core::log::level(),
            ..Default::default()
        },
        |engine| engine.config().app,
    )
}

/// 换配置文件前的准备：文件已存在就读出来，不存在就把当前配置存过去（换个地方放）。
/// 读不出来或存不进去都在这里报错，此时还什么都没改。
fn prepare_config(path: &Path) -> aoproxy_core::Result<Config> {
    if path.exists() {
        return Config::load(path);
    }
    let current = shared::engine().map_or_else(Config::default_empty, |e| e.config());
    current.save(path)?;
    Ok(current)
}

/// 规则存不下去的原因，`None` 表示可以存。
///
/// 与保存时引擎做的是同一套核心校验，只是提前在对话框关闭之前同步做完：
/// 保存本身是异步的，那时对话框已经关了，错误只能挂到主窗口的提示条上。
fn rule_problem(json: &str, is_new: bool) -> Option<String> {
    let rule = match serde_json::from_str::<aoproxy_core::Rule>(json) {
        Ok(rule) => rule,
        Err(e) => return Some(form_error(&e)),
    };
    let Some(engine) = shared::engine() else {
        return Some(i18n::tr("gui.no_config").to_owned());
    };
    let mut draft = engine.config();
    if is_new && draft.rule(&rule.id).is_some() {
        return Some(Error::DuplicateRuleId(rule.id).localized());
    }
    // 证书与私钥是否配对要读盘解析，只在保存前做，不混进每次的 validate。
    let tls = rule.tls.clone();
    draft.put_rule(rule);
    let checked = draft
        .validate()
        .and_then(|()| tls.map_or(Ok(()), |tls| tls.verify_pair()));
    checked.err().map(|e| e.localized())
}

fn path_qstring(path: &Path) -> QString {
    QString::from(path.display().to_string().as_str())
}

/// 表单数据解析失败的文案。
///
/// 这类失败不走 `aoproxy_core::Error`：核心的错误类型描述的是配置文件与运行期的
/// 问题，而这里坏掉的是 QML 传过来的 JSON——属于桥接层自己的事，硬塞进
/// `ConfigParse`（它要的是文件路径和 TOML 错误）只会让文案不知所云。
fn form_error(e: &serde_json::Error) -> String {
    i18n::tr_args("gui.form_invalid", &[("reason", &e.to_string())])
}

/// 新建规则的默认值：未启用、监听回环、反向模式、直连出网。
///
/// 回环而非 `0.0.0.0`：新规则默认只对本机开放，用户想暴露到公网得自己改，
/// 免得一条还没配认证的规则一存下来就对外监听。未启用与配置文件里不写 `enabled`
/// 的默认一致：没被明确打开过的规则不会被「全部启用」或开机自启带起来。
fn default_rule() -> aoproxy_core::Rule {
    aoproxy_core::Rule {
        id: String::new(),
        name: String::new(),
        enabled: false,
        mode: aoproxy_core::Mode::Reverse,
        listen: "127.0.0.1:8080".to_owned(),
        target: None,
        upstream: aoproxy_core::Upstream::default(),
        tls: None,
        auth: aoproxy_core::AuthConfig::default(),
    }
}

//! 主桥接对象 `AppBridge`。
//!
//! 持有 [`Engine`] 实例，向 QML 暴露：
//! - `configPath` 属性（只读，字符串）
//! - `startRule(id)` / `stopRule(id)` 可调用方法
//! - `statusChanged()` 信号，规则状态变化时触发

use std::pin::Pin;
use std::sync::Arc;

use cxx_qt_lib::QString;

use aoproxy_core::Engine;

use super::shared;

// ─────────────────────── Rust 状态 ───────────────────────

/// AppBridge 的 Rust 侧字段（cxx-qt 要求 Data struct 实现 Default）。
pub struct AppBridgeState {
    engine: Option<Arc<Engine>>,
    /// 类型必须与 `#[qproperty(QString, config_path)]` 声明一致。
    config_path: QString,
    /// 运行中的规则数，驱动托盘图标两态与悬停提示。
    running_count: i32,
    /// 最近一次操作的错误文案，空串表示无错误。
    last_error: QString,
    /// 托盘是否可用。Linux 上检测不到 StatusNotifier 时为 false，
    /// 关闭窗口改为最小化，避免程序藏进不存在的托盘里找不回来。
    tray_available: bool,
    /// 当前语言标签，仅用于让 QML 的 `tr()` 绑定在语言切换后失效重算。
    language: QString,
    /// 程序版本，取自 `aoproxy_core::VERSION`（即 Cargo.toml 里的版本号）。
    /// 走属性而不是让 QML 写死：版本号只该有一处来源，
    /// 界面上的和 `aoproxy --version` 报的必须是同一个。
    version: QString,
}

impl Default for AppBridgeState {
    fn default() -> Self {
        // 引擎与配置路径在 main 中初始化一次，这里只取引用，
        // 确保与 RuleModel / LogModel 看到的是同一个实例。
        let engine = shared::engine();
        let running_count = engine.as_ref().map_or(0, |e| e.running_count() as i32);
        Self {
            engine,
            config_path: QString::from(shared::config_path().as_str()),
            running_count,
            last_error: QString::default(),
            tray_available: super::tray_available(),
            language: QString::from(aoproxy_core::i18n::language().tag()),
            version: QString::from(aoproxy_core::VERSION),
        }
    }
}

// ─────────────────────── cxx-qt 桥接 ───────────────────────

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
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
        type AppBridge = super::AppBridgeState;

        /// 规则状态发生变化（启动/停止/失败）时发出，QML 监听后刷新列表。
        /// QML 侧对应 `onStatusChanged`。
        #[qsignal]
        #[cxx_name = "statusChanged"]
        fn status_changed(self: Pin<&mut AppBridge>);

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

        /// 新增或按 ID 覆盖规则。`json` 为编辑对话框序列化出的规则对象；
        /// 校验失败时不写盘，错误经 `lastError` 反馈。
        #[qinvokable]
        #[cxx_name = "saveRule"]
        fn save_rule(self: Pin<&mut AppBridge>, json: &QString);

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

        /// 深度校验一条规则的 TLS 证书与私钥是否配对。
        /// 返回空串表示通过，否则是错误文案。
        /// 保存前单独调用：解析密钥要读盘，不该混进每次 `validate`。
        #[qinvokable]
        #[cxx_name = "verifyTls"]
        fn verify_tls(self: &AppBridge, cert: &QString, key: &QString) -> QString;
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

    /// 保存规则。JSON 解析失败与配置校验失败走同一条反馈路径：
    /// 都是"这份表单不能存"，界面只需要一句话说明原因。
    fn save_rule(self: Pin<&mut Self>, json: &QString) {
        let text = json.to_string();
        let rule = match serde_json::from_str::<aoproxy_core::Rule>(&text) {
            Ok(r) => r,
            Err(e) => {
                self.report_text(form_error(&e));
                return;
            }
        };
        self.spawn_rule_op(move |engine| async move { engine.upsert_rule(rule).await });
    }

    fn delete_rule(self: Pin<&mut Self>, id: &QString) {
        let id_str = id.to_string();
        self.spawn_rule_op(move |engine| async move { engine.remove_rule(&id_str).await });
    }

    /// 取单条规则的 JSON。ID 为空或查不到时返回新建用的模板，
    /// 这样编辑对话框只有一条填充路径，不必区分新建与修改。
    fn rule_json(&self, id: &QString) -> QString {
        let id_str = id.to_string();
        let rule = self
            .engine
            .as_ref()
            .and_then(|e| e.config().rule(&id_str).cloned())
            .unwrap_or_else(default_rule);
        QString::from(serde_json::to_string(&rule).unwrap_or_default().as_str())
    }

    fn app_config_json(&self) -> QString {
        let app = self
            .engine
            .as_ref()
            .map(|e| e.config().app)
            .unwrap_or_default();
        QString::from(serde_json::to_string(&app).unwrap_or_default().as_str())
    }

    /// 应用全局设置。语言、日志开关与级别由核心即时生效，
    /// 随后发 `languageChanged` 让 QML 重新求值所有 `tr()` 绑定。
    fn save_app_config(mut self: Pin<&mut Self>, json: &QString) {
        let text = json.to_string();
        let app = match serde_json::from_str::<aoproxy_core::AppConfig>(&text) {
            Ok(a) => a,
            Err(e) => {
                self.report_text(form_error(&e));
                return;
            }
        };
        let result = match self.as_ref().engine.clone() {
            Some(engine) => engine.update_app_config(app),
            None => Ok(()),
        };
        self.as_mut().report(result);
        // 先更新 language 属性再发信号：属性变化会让 QML 里所有经辅助函数
        // 取的文案重算，信号则供需要手动刷新的地方（如托盘菜单）使用。
        let tag = QString::from(aoproxy_core::i18n::language().tag());
        self.as_mut().set_language(tag);
        self.as_mut().language_changed();
        self.status_changed();
    }

    fn tr(&self, key: &QString) -> QString {
        QString::from(aoproxy_core::i18n::tr(&key.to_string()))
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
        QString::from(aoproxy_core::i18n::tr_args(&key.to_string(), &args).as_str())
    }

    /// 占位符全部按字符串替换，数字由 QML 侧 `JSON.stringify` 自然转成
    /// 数字字面量，这里统一按文本取。解析不了就退回无参词条，
    /// 让界面上出现原文而不是空白。
    fn tr_args(&self, key: &QString, args: &QString) -> QString {
        let key = key.to_string();
        let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            &args.to_string(),
        ) else {
            return QString::from(aoproxy_core::i18n::tr(&key));
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
        QString::from(aoproxy_core::i18n::tr_args(&key, &pairs).as_str())
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

    /// 证书与私钥是否配对。校验走核心的 [`TlsConfig::verify_pair`]，
    /// 与 `aoproxy config check` 用的是同一条路径，结论不会两处不一致。
    ///
    /// [`TlsConfig::verify_pair`]: aoproxy_core::config::TlsConfig::verify_pair
    fn verify_tls(&self, cert: &QString, key: &QString) -> QString {
        let cert = cert.to_string();
        let key = key.to_string();
        if cert.is_empty() && key.is_empty() {
            return QString::default();
        }
        let tls = aoproxy_core::config::TlsConfig {
            cert: cert.into(),
            key: key.into(),
        };
        let text = match tls.verify_pair() {
            Ok(()) => String::new(),
            Err(e) => e.to_string(),
        };
        QString::from(text.as_str())
    }

    /// 退出：先停全部规则，等监听端口干净释放后再让 QML 关掉应用。
    fn quit(self: Pin<&mut Self>) {
        let engine = self.as_ref().engine.clone();
        let qt_thread = self.qt_thread();
        tokio::spawn(async move {
            if let Some(engine) = engine {
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
        let Some(engine) = self.as_ref().engine.clone() else {
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
            Err(e) => e.to_string(),
        };
        self.report_text(text);
    }

    /// 直接写入一条错误文案。空串表示"没有错误"，界面据此收起提示条。
    fn report_text(self: Pin<&mut Self>, text: String) {
        self.set_last_error(QString::from(text.as_str()));
    }

    fn refresh_running_count(self: Pin<&mut Self>) {
        let count = self.as_ref().engine.as_ref().map_or(0, |e| e.running_count() as i32);
        self.set_running_count(count);
    }
}

/// 表单数据解析失败的文案。
///
/// 这类失败不走 `aoproxy_core::Error`：核心的错误类型描述的是配置文件与运行期的
/// 问题，而这里坏掉的是 QML 传过来的 JSON——属于桥接层自己的事，硬塞进
/// `ConfigParse`（它要的是文件路径和 TOML 错误）只会让文案不知所云。
fn form_error(e: &serde_json::Error) -> String {
    aoproxy_core::i18n::tr_args("gui.form_invalid", &[("reason", &e.to_string())])
}

/// 新建规则的默认值：监听回环、反向模式、直连出网。
///
/// 回环而非 `0.0.0.0`：新规则默认只对本机开放，用户想暴露到公网得自己改，
/// 免得一条还没配认证的规则一存下来就对外监听。
fn default_rule() -> aoproxy_core::Rule {
    aoproxy_core::Rule {
        id: String::new(),
        name: String::new(),
        enabled: true,
        mode: aoproxy_core::Mode::Reverse,
        listen: "127.0.0.1:8080".to_owned(),
        target: None,
        upstream: aoproxy_core::Upstream::default(),
        tls: None,
        auth: aoproxy_core::AuthConfig::default(),
    }
}

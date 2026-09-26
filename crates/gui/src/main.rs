//! AOProxy GUI 入口。
//!
//! 主线程运行 Qt 事件循环；tokio 运行时在独立线程池中运行，
//! 通过 channel 与 Qt 侧通信，保证 GUI 不阻塞。
//!
//! release 构建声明 Windows 子系统，启动时不再附带控制台窗口。
//! 代价是 stderr 无处可去（Qt 自身的 qpa 告警也随之消失），
//! 程序日志改由 [`LogBuffer`] 环形缓冲承载，在界面日志面板里看。
//! debug 构建保留控制台，方便开发期直接看 Qt 的诊断输出。
//!
//! 命令行参数（Qt 自己的参数如 `-platform` 由 Qt 处理）：
//!
//! - `-c <FILE>` / `--config <FILE>`：本次运行用这个配置文件，不改设置页记下的位置。
//! - `--autostart`：开机自启时由系统带上，见 [`aoproxy_core::autostart`]。
//!
//! [`LogBuffer`]: aoproxy_core::log::LogBuffer
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod single_instance;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QQuickStyle, QString, QUrl};

use aoproxy_core::{autostart, config, i18n, Config, Engine as AoEngine, Error, Language};

fn main() {
    let args = LaunchArgs::parse(std::env::args_os().skip(1));

    // 单实例守卫放在最前面：抢不到锁说明已有窗口在跑，把它唤到前台然后安静退出。
    // 早于 runtime 与 Qt 的一切初始化——第二次启动不该闪一下窗口再消失。
    let show_rx = match single_instance::acquire() {
        single_instance::Acquire::Primary(rx) => rx,
        single_instance::Acquire::Secondary => return,
    };

    // tokio runtime 必须在 Qt 事件循环之前启动，供桥接对象使用。
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    // 将 runtime 句柄存入线程局部，供桥接回调取用。
    let _guard = rt.enter();

    // 必须在创建 QGuiApplication 之前设定：Windows 上 Qt 6 默认用原生
    // Windows 样式，它不允许定制 Button/CheckBox 的 background 与
    // contentItem，我们的深色自绘界面会被忽略并打印告警。
    QQuickStyle::set_style(&QString::from("Basic"));

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    // 先按系统语言，读到配置后再换成配置里的：加载出错时的提示也得有个语言。
    i18n::set_language(Language::detect());

    // 配置文件：命令行 `--config` > 设置页记下的位置 > 平台默认。
    // 位置文件坏了就退回默认位置，同时把错误挂出来，而不是连界面都起不来。
    let default_config_path = config::config_path().unwrap_or_default();
    let (config_path, location_error) = match config::resolve_config_path(args.config.as_deref()) {
        Ok(path) => (path, None),
        Err(e) => (default_config_path.clone(), Some(e)),
    };
    let cfg = load_config(&config_path);

    // 语言跟着配置走：设置页里选了什么，下次启动就是什么，
    // 否则下拉框里写着 English，界面却还是按系统语言显示的中文。
    if let Ok(c) = &cfg {
        i18n::set_language(c.app.language);
    }

    // 初始化日志（GUI 模式：写 stderr 同时写环形缓冲）
    let (log_buf, log_rx) = aoproxy_core::log::LogBuffer::new();
    let (log_enabled, log_level) = match &cfg {
        Ok(c) => (c.app.logging_enabled, c.app.log_level),
        Err(_) => (true, aoproxy_core::config::LogLevel::Info),
    };
    aoproxy_core::log::init(log_enabled, log_level, Some(log_buf.clone()));

    let startup_error = location_error.as_ref().or(cfg.as_ref().err()).map(Error::localized);
    if let Some(e) = &startup_error {
        tracing::error!("{e}");
    }

    // 三个桥接对象由 QML 各自实例化，必须共享同一份引擎与日志缓冲。
    //
    // 引擎带上配置路径，界面里增删规则、改设置才能落盘；只用 `new` 的话
    // 改动只活在内存里，重启就没了。
    bridge::shared::init(
        cfg.ok()
            .map(|c| Arc::new(AoEngine::with_config_path(c, &config_path))),
        log_buf,
        log_rx,
        show_rx,
        bridge::shared::Launch {
            config_path,
            default_config_path,
            launched_at_login: args.autostart,
            startup_error,
        },
    );

    // 加载 QML 主界面（通过 cxx-qt-build 打包到资源系统）
    // 注意：cxx-qt-lib 0.7 没有绑定 rootObjects()，无法在此判断加载是否成功；
    // QML 出错时 Qt 会自行往 stderr 打印诊断，届时窗口不会出现。
    //
    // /qt/qml 前缀由 cxx-qt-build 生成的 qrc 决定，是 Qt 6 QML 模块的约定。
    // 写成 qrc:/AOProxy/qml/main.qml 会在运行时报"找不到文件或目录"。
    engine
        .pin_mut()
        .load(&QUrl::from("qrc:/qt/qml/AOProxy/qml/main.qml"));

    // exec() 返回 Qt 的退出码，直接作为进程退出码。
    std::process::exit(app.pin_mut().exec());
}

/// 读配置。文件还不存在算正常：从空配置起步，第一次改动时写盘；
/// 界面语言此时跟随系统，并记进配置，设置页的下拉框才与界面一致。
fn load_config(path: &std::path::Path) -> aoproxy_core::Result<Config> {
    match Config::load(path) {
        Err(Error::ConfigRead { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            let mut fresh = Config::default_empty();
            fresh.app.language = Language::detect();
            Ok(fresh)
        }
        other => other,
    }
}

/// 本程序认的命令行参数。认不出的一律忽略：其中有 Qt 自己的参数。
#[derive(Default)]
struct LaunchArgs {
    /// `-c <FILE>` / `--config <FILE>` / `--config=<FILE>`，相对路径按当前目录补全。
    config: Option<PathBuf>,
    /// [`autostart::LAUNCH_FLAG`]。
    autostart: bool,
}

impl LaunchArgs {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut out = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let path = if arg == autostart::LAUNCH_FLAG {
                out.autostart = true;
                None
            } else if arg == "-c" || arg == "--config" {
                args.next().map(PathBuf::from)
            } else {
                arg.to_str()
                    .and_then(|s| s.strip_prefix("--config="))
                    .map(PathBuf::from)
            };
            if let Some(path) = path {
                // 设置页上要显示完整路径；工作目录之后也可能变，趁现在补全。
                out.config = Some(std::path::absolute(&path).unwrap_or(path));
            }
        }
        out
    }
}

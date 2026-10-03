//! 无引擎时的设置恢复回归。直接复用共享上下文，无需启动 Qt 界面。
//!
//! 一个测试独占本测试程序的进程级状态，覆盖配置损坏到引擎恢复的整个过程。

#[allow(dead_code)]
#[path = "../src/bridge/mod.rs"]
mod bridge;

// build.rs links the generated QML plugin into integration tests too. Include its
// Rust bridges so those C++ entry points resolve, even though no UI is created.
use bridge::shared;

use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};

use aoproxy_core::{AppConfig, Config, Engine, Language, LogLevel};

fn assert_app(actual: &AppConfig, expected: &AppConfig) {
    assert_eq!(actual.language, expected.language);
    assert_eq!(actual.logging_enabled, expected.logging_enabled);
    assert_eq!(actual.log_level, expected.log_level);
    assert_eq!(actual.minimize_to_tray, expected.minimize_to_tray);
}

#[test]
fn recovery_preserves_settings_and_then_uses_engine_config() {
    // main 在配置损坏时临时打开日志，共享设置应继承实际日志初值。
    aoproxy_core::log::set_logging_enabled(true);
    aoproxy_core::log::set_level(LogLevel::Debug);
    let (buffer, log_rx) = aoproxy_core::log::LogBuffer::new();
    let (_show_tx, show_rx) = mpsc::channel();
    shared::init(
        None,
        buffer,
        log_rx,
        show_rx,
        shared::Launch {
            config_path: "broken.toml".into(),
            default_config_path: "default.toml".into(),
            launched_at_login: false,
            startup_error: Some("invalid config".into()),
        },
    );
    let initial = shared::current_config().app;
    assert_eq!(initial.language, None);
    assert!(initial.logging_enabled);
    assert_eq!(initial.log_level, LogLevel::Debug);
    assert!(initial.minimize_to_tray);

    // 配置尚未恢复时修改设置，尤其是无法从日志/语言全局状态重建的托盘选项。
    let mut chosen = AppConfig {
        language: Some(Language::ZhCn),
        logging_enabled: false,
        log_level: LogLevel::Trace,
        minimize_to_tray: false,
    };
    shared::set_fallback_app(chosen.clone());
    assert_app(&shared::current_config().app, &chosen);
    chosen.language = None;
    shared::set_fallback_app(chosen.clone());
    assert_app(&shared::current_config().app, &chosen);

    // prepare_config 的新文件路径使用这个完整快照，保存重读后仍保留「跟随系统」。
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aoproxy-recovery-settings-{}-{unique}.toml",
        std::process::id()
    ));
    let config = shared::current_config();
    assert!(config.rules.is_empty());
    config.save(&path).unwrap();
    let saved = Config::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_app(&saved.app, &chosen);

    // 装上引擎后，真实配置必须覆盖此前的临时设置；后续也不能读回旧 fallback。
    let recovered = AppConfig {
        language: Some(Language::EnUs),
        logging_enabled: true,
        log_level: LogLevel::Info,
        minimize_to_tray: true,
    };
    let mut recovered_config = Config::default_empty();
    recovered_config.app = recovered.clone();
    shared::set_engine(Arc::new(Engine::new(recovered_config)));
    assert_app(&shared::current_config().app, &recovered);
    shared::set_fallback_app(chosen);
    assert_app(&shared::current_config().app, &recovered);
}

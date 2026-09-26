//! cxx-qt 桥接模块：将引擎状态和控制暴露给 QML。

pub mod app;
pub mod log_model;
pub mod rule_model;
pub mod shared;

/// 本机是否有可用的系统托盘。
///
/// Windows 与 macOS 始终有。Linux 上托盘走 StatusNotifier（D-Bus），
/// 有些精简桌面没有实现，此时 `SystemTrayIcon` 会静默失败——窗口一关就
/// 再也叫不回来。检测不到就让界面退回"关闭即退出"，并提示一次。
///
/// 判据是会话类型而非真去 D-Bus 上问：`Qt.labs.platform` 的可用性在
/// 运行期才能知道，而这个属性要在窗口出现之前就定下来。有 Wayland 或
/// X11 会话即认为桌面环境完整，纯 tty（如 SSH 转发）则认为没有托盘。
pub fn tray_available() -> bool {
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
    }
}

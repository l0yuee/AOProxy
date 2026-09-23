// 系统托盘图标与右键菜单。
//
// 用 Qt.labs.platform 而不是 QtQuick.Controls：托盘与原生菜单只有这个模块提供，
// cxx-qt-lib 0.7 也没有 QSystemTrayIcon 绑定。注意本文件不能再 import
// QtQuick.Controls——两边都有 Menu/MenuItem，同名类型会撞。
//
// 图标走 qrc：release 是 windows 子系统，工作目录不确定，相对路径取不到文件。
//
// QtQml 是为了 `Component.onCompleted`——那个附加对象由 QtQml 提供，
// 只导 Qt.labs.platform 会在运行期报 "Non-existent attached object"。
// QtQml 不带 Menu/MenuItem，不会与 Qt.labs.platform 撞名。
import Qt.labs.platform 1.1
import QtQml 2.15

SystemTrayIcon {
    id: root

    property var bridge: null

    /// 双击图标或点「显示主窗口」。
    signal showWindowRequested()
    /// 点「退出」。确认对话框由主窗口负责弹，这里只转达意图。
    signal quitRequested()

    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }

    readonly property int runningCount: bridge ? bridge.runningCount : 0

    // Linux 上没有托盘协议时 available 为 false，此时不显示，
    // 由主窗口按 trayAvailable 提示一次「关闭窗口只会最小化」。
    visible: available

    icon.source: runningCount > 0
                 ? "qrc:/qt/qml/AOProxy/qml/icons/tray-active.ico"
                 : "qrc:/qt/qml/AOProxy/qml/icons/tray-idle.ico"

    tooltip: runningCount > 0
             ? (bridge ? bridge.trFmt("gui.tray_tip_running",
                                      JSON.stringify({ count: runningCount })) : "")
             : t("gui.tray_tip_idle")

    // 托盘能力是运行时才知道的，交给桥对象，界面各处统一读 trayAvailable。
    Component.onCompleted: if (bridge) bridge.trayAvailable = available

    onActivated: function (reason) {
        // Trigger 是单击，Windows 上惯例是双击；两个都接。
        if (reason === SystemTrayIcon.Trigger || reason === SystemTrayIcon.DoubleClick)
            root.showWindowRequested()
    }

    menu: Menu {
        MenuItem {
            text: root.t("gui.show_window")
            onTriggered: root.showWindowRequested()
        }

        MenuSeparator {}

        MenuItem {
            text: root.t("gui.start_all")
            onTriggered: if (root.bridge) root.bridge.startAll()
        }

        MenuItem {
            text: root.t("gui.stop_all")
            onTriggered: if (root.bridge) root.bridge.stopAll()
        }

        MenuSeparator {}

        MenuItem {
            text: root.t("gui.quit")
            onTriggered: root.quitRequested()
        }
    }
}

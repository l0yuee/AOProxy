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
// 菜单里按规则、按日志级别生成菜单项用的 Instantiator 也来自它（QtQml.Models）。
// QtQml 不带 Menu/MenuItem，不会与 Qt.labs.platform 撞名。
//
// 菜单结构：
//
//     显示主窗口
//     ──────────
//     规则 · 运行中 1 / 3 ▸     ← 子菜单：每条规则一项，勾选即「启用」开关
//     全部启用 / 全部停止
//     ──────────
//     日志 · info ▸           ← 子菜单：[✓] 启用日志、各级别
//     ──────────
//     退出
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
    function tf(key, args) {
        return bridge ? (bridge.language, bridge.trFmt(key, JSON.stringify(args))) : ""
    }

    readonly property int runningCount: bridge ? bridge.runningCount : 0

    /// 菜单里的规则：`{id, name, enabled, status}` 数组，取自 bridge.ruleMenuJson()。
    ///
    /// 不拿主窗口的 RuleModel 当数据源：它为了刷新流量每两秒整表重置一次，
    /// 菜单项会跟着销毁重建，菜单开着时可能闪动甚至被系统收起。这里只在规则的
    /// 名字、开关或状态真变了的时候才换数据。
    property var rules: []
    property string rulesJson: ""

    readonly property var logLevels: ["error", "warn", "info", "debug", "trace"]

    function syncRules() {
        if (!bridge)
            return
        var json = bridge.ruleMenuJson()
        if (json === rulesJson)
            return
        rulesJson = json
        rules = JSON.parse(json)
    }

    /// 日志子菜单的勾选照 bridge 的属性重设一遍。
    ///
    /// 不写成 `checked: …` 绑定：点菜单项时 Qt 先自行翻转勾选再发 triggered，
    /// 若 bridge 的值没变（比如点了本来就选中的级别），绑定不会重算，勾就停在错的状态上。
    function syncLogMenu() {
        if (!bridge)
            return
        logSwitch.checked = bridge.loggingEnabled
        for (var i = 0; i < levelItems.count; i++) {
            var item = levelItems.objectAt(i)
            item.checked = item.level === bridge.logLevel
        }
    }

    /// 原生菜单把 `&` 当助记符前缀（Windows 上 `A&B` 会显示成带下划线的 `AB`），
    /// 规则名里的 `&` 要双写。
    function menuText(text) { return text.replace(/&/g, "&&") }

    // Linux 上没有托盘协议时 available 为 false，此时不显示，
    // 由主窗口按 trayAvailable 提示一次「关闭窗口即退出」。
    visible: available

    icon.source: runningCount > 0
                 ? "qrc:/qt/qml/AOProxy/qml/icons/tray-active.ico"
                 : "qrc:/qt/qml/AOProxy/qml/icons/tray-idle.ico"

    tooltip: runningCount > 0
             ? (bridge ? bridge.trFmt("gui.tray_tip_running",
                                      JSON.stringify({ count: runningCount })) : "")
             : t("gui.tray_tip_idle")

    Component.onCompleted: {
        // 托盘能力是运行时才知道的，交给桥对象，界面各处统一读 trayAvailable。
        if (bridge)
            bridge.trayAvailable = available
        syncRules()
        syncLogMenu()
    }

    onActivated: function (reason) {
        // Trigger 是单击，Windows 上惯例是双击；两个都接。
        if (reason === SystemTrayIcon.Trigger || reason === SystemTrayIcon.DoubleClick)
            root.showWindowRequested()
    }

    menu: Menu {
        id: trayMenu

        MenuItem {
            text: root.t("gui.show_window")
            onTriggered: root.showWindowRequested()
        }

        MenuSeparator {}

        // 规则收在子菜单里，规则一多主菜单也不会拉得很长；标题上带着运行计数，
        // 不展开也能看个大概。子菜单在 Linux 上启动时的那行告警见下面日志子菜单的说明。
        Menu {
            id: rulesMenu
            title: root.t("gui.rules") + " · "
                   + root.tf("gui.running_count", { count: root.runningCount,
                                                    total: root.rules.length })

            // 规则各项由下面的 Instantiator 插在这一项前面。没有规则时由它占位，
            // 免得子菜单空着，像是坏了。
            MenuItem {
                visible: root.rules.length === 0
                enabled: false
                text: root.t("gui.tray_no_rules")
            }
        }

        MenuItem {
            text: root.t("gui.start_all")
            onTriggered: if (root.bridge) root.bridge.startAll()
        }

        MenuItem {
            text: root.t("gui.stop_all")
            onTriggered: if (root.bridge) root.bridge.stopAll()
        }

        MenuSeparator {}

        // 与设置页的日志开关、级别是同一份设置，这边改了那边跟着变。
        //
        // Linux 上（没有 Qt Widgets、托盘走 D-Bus）启动时 stderr 会有一行
        // "ERROR: No native Menu implementation available"，来自这个子菜单，无害：
        // QML 按创建的逆序完成组件，子菜单先于父菜单完成，头一次建原生菜单时父菜单
        // 还没有句柄，只好退到 Widgets 那条路并打出这句；父菜单随后同步各项时会把它
        // 重新建好（Qt.labs.platform 的 QQuickLabsPlatformMenuItem::sync）。
        // 这是 Qt.labs.platform 里任何静态嵌套子菜单的通病，Windows 与 macOS 不会出现。
        Menu {
            id: logMenu
            title: root.t("gui.logs") + " · "
                   + (root.bridge && root.bridge.loggingEnabled ? root.bridge.logLevel
                                                                : root.t("gui.log_off"))

            MenuItem {
                id: logSwitch
                text: root.t("gui.logging")
                checkable: true
                onTriggered: {
                    if (root.bridge)
                        root.bridge.enableLogging(checked)
                    root.syncLogMenu()
                }
            }

            MenuSeparator {}

            // 级别各项由 levelItems 插在这里（第 2 项起）。

            MenuItemGroup { id: levelGroup }
        }

        MenuSeparator {}

        MenuItem {
            text: root.t("gui.quit")
            onTriggered: root.quitRequested()
        }

        Instantiator {
            model: root.rules

            delegate: MenuItem {
                required property var modelData

                checkable: true
                // 勾选跟规则卡片上的「启用」开关是同一个：打开即启动，关上即停止。
                // 取的是配置里存下的开关，重启后照旧；配置里没写的规则算未启用。
                checked: modelData.enabled
                // 启动中的规则等它有个结果再说，免得连点两下叠出两次操作。
                enabled: modelData.status !== "starting"
                // 名字之外还带上运行状态：开关开着的规则也可能没在跑（端口被占、启动失败）。
                // 状态文案经 t() 取，语言切换时自动重算。
                text: root.menuText(modelData.name || modelData.id)
                      + " · " + root.t("state." + modelData.status)

                onTriggered: {
                    if (!root.bridge)
                        return
                    // 清掉缓存：操作完成后无论成败都照实重建一遍菜单项。
                    // 否则操作失败、数据没变时，勾选会停在刚被点过的状态上。
                    root.rulesJson = ""
                    root.bridge.setRuleEnabled(modelData.id, checked)
                }
            }

            onObjectAdded: function (index, object) { rulesMenu.insertItem(index, object) }
            onObjectRemoved: function (index, object) { rulesMenu.removeItem(object) }
        }

        Instantiator {
            id: levelItems
            model: root.logLevels

            delegate: MenuItem {
                required property string modelData
                readonly property string level: modelData

                // 级别名是约定俗成的英文标识，不做翻译，与设置页一致。
                text: level
                checkable: true
                group: levelGroup
                // 日志关着时级别不起作用，与设置页一样置灰。
                enabled: root.bridge ? root.bridge.loggingEnabled : false

                onTriggered: {
                    if (root.bridge)
                        root.bridge.selectLogLevel(level)
                    root.syncLogMenu()
                }
            }

            onObjectAdded: function (index, object) {
                logMenu.insertItem(2 + index, object)
                root.syncLogMenu()
            }
            onObjectRemoved: function (index, object) { logMenu.removeItem(object) }
        }

        Connections {
            target: root.bridge
            // 规则增删、启停、开关切换都会发 statusChanged；换配置文件发 configChanged。
            function onStatusChanged() { root.syncRules() }
            function onConfigChanged() { root.syncRules() }
            function onLoggingEnabledChanged() { root.syncLogMenu() }
            function onLogLevelChanged() { root.syncLogMenu() }
        }
    }
}

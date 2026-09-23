// 设置页。改动即时生效：每个控件一变就把整份 AppConfig 发下去，
// 核心那边负责落盘并立刻应用（语言、日志开关与级别都无需重启）。
//
// 没有「保存」按钮是刻意的：这几项都是开关与下拉，没有「填一半」的中间态，
// 多一步确认只是多一次点击。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

Item {
    id: root

    property var bridge: null

    /// 读取期间屏蔽回写，否则 currentIndex 一被赋值就会把旧值存回去。
    property bool loading: true
    property var langTags: []
    property var langNames: []

    readonly property var levels: ["error", "warn", "info", "debug", "trace"]

    Theme { id: theme }

    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }

    function load() {
        loading = true
        var items = JSON.parse(bridge.languageList())
        var tags = [], names = []
        for (var i = 0; i < items.length; i++) {
            tags.push(items[i].tag)
            names.push(items[i].name)
        }
        langTags = tags
        langNames = names
        var cfg = JSON.parse(bridge.appConfigJson())
        langBox.currentIndex = Math.max(0, tags.indexOf(cfg.language))
        logBox.checked = cfg.logging_enabled === true
        levelBox.currentIndex = Math.max(0, levels.indexOf(cfg.log_level || "info"))
        trayBox.checked = cfg.minimize_to_tray !== false
        loading = false
    }

    /// 字段名必须与 AppConfig 完全一致：那边是 deny_unknown_fields，
    /// 多一个键、少一个键都会被整份拒掉。
    function apply() {
        if (loading || !bridge) return
        bridge.saveAppConfig(JSON.stringify({
            language: langTags[langBox.currentIndex],
            logging_enabled: logBox.checked,
            log_level: levels[levelBox.currentIndex],
            minimize_to_tray: trayBox.checked
        }))
    }

    Component.onCompleted: load()

    ScrollView {
        id: scroll
        anchors.fill: parent
        anchors.margins: theme.pad
        clip: true
        contentWidth: availableWidth

        ColumnLayout {
            width: scroll.availableWidth
            spacing: theme.gap

            FieldRow {
                label: root.t("gui.language")
                Layout.fillWidth: true
                Dropdown {
                    id: langBox
                    // 语言项显示各自的本族名，界面语言看不懂时也能找到自己那一项，
                    // 所以这里不需要 labels。
                    model: root.langNames
                    Layout.preferredWidth: 190
                    onActivated: root.apply()
                }
                Item { Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.log_level")
                Layout.fillWidth: true
                ToggleBox {
                    id: logBox
                    text: root.t("gui.logging")
                    onToggled: root.apply()
                }
                Dropdown {
                    id: levelBox
                    // 级别名是约定俗成的英文标识，不做翻译。
                    model: root.levels
                    enabled: logBox.checked
                    opacity: enabled ? 1.0 : theme.disabledOpacity
                    Layout.leftMargin: theme.gap
                    Layout.preferredWidth: 130
                    onActivated: root.apply()
                }
                Item { Layout.fillWidth: true }
            }

            FieldRow {
                label: ""
                hint: root.bridge && !root.bridge.trayAvailable ? root.t("gui.tray_missing") : ""
                Layout.fillWidth: true
                ToggleBox {
                    id: trayBox
                    text: root.t("gui.minimize_to_tray")
                    enabled: root.bridge ? root.bridge.trayAvailable : true
                    Layout.fillWidth: true
                    onToggled: root.apply()
                }
            }

            Rectangle {
                implicitHeight: 1
                color: theme.border
                Layout.fillWidth: true
                Layout.topMargin: theme.gap
                Layout.bottomMargin: theme.gap / 2
            }

            FieldRow {
                label: root.t("gui.config_path")
                Layout.fillWidth: true
                LineInput {
                    // 只读但可选中：让人能把路径复制出去，比加一个「打开目录」按钮省事。
                    text: root.bridge ? root.bridge.configPath : ""
                    readOnly: true
                    Layout.fillWidth: true
                }
            }

            // 版本号。窗口标题与顶栏徽标上都有，这里再放一份是因为
            // 「去设置里找版本号」是大多数人的第一反应，报问题时也方便复制。
            FieldRow {
                label: root.t("gui.version")
                Layout.fillWidth: true
                LineInput {
                    text: root.bridge ? root.bridge.version : ""
                    readOnly: true
                    font.family: theme.mono
                    Layout.fillWidth: true
                }
            }
        }
    }
}

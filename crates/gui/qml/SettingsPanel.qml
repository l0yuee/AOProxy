// 设置页。改动即时生效：每个控件一变就把整份 AppConfig 发下去，
// 核心那边负责落盘并立刻应用（语言、日志开关与级别都无需重启）。
//
// 没有「保存」按钮是刻意的：这几项都是开关与下拉，没有「填一半」的中间态，
// 多一步确认只是多一次点击。配置文件路径例外：它是个要敲完的输入框，
// 敲到一半的路径不该被当成要换的文件，所以由「应用」或回车确认。
//
// 开机自启也不在 AppConfig 里：它的状态就是系统里登记着什么，
// 每次都直接去问系统（见 aoproxy_core::autostart）。
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
    /// 配置文件那一栏的错误，空串表示没有。
    property string pathError: ""

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
        autostartBox.checked = bridge.autostartSupported && bridge.autostartEnabled()
        pathField.text = bridge.configPath
        pathError = ""
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

    /// 换配置文件。路径或文件本身的问题当场返回、显示在输入框下面；
    /// 切换完成后 bridge 发 configChanged，整页重读。
    function applyPath(path) {
        if (!bridge) return
        pathError = bridge.applyConfigPath(path)
    }

    Component.onCompleted: load()

    // 开机自启也可能在系统那边被改（任务管理器、「启动应用程序」）：每次翻到本页都重新问一次。
    onVisibleChanged: {
        if (visible && bridge && !loading)
            autostartBox.checked = bridge.autostartSupported && bridge.autostartEnabled()
    }

    Connections {
        target: root.bridge
        // 换了配置文件：语言、日志、托盘这几项都随新文件变了，整页重读。
        function onConfigChanged() { root.load() }
    }

    // 文件对话框依赖 QtQuick.Dialogs，单独成文件经 Loader 装载，理由见 ConfigFileDialog.qml。
    Loader {
        id: dialogLoader
        Component.onCompleted: setSource("ConfigFileDialog.qml")

        onLoaded: {
            item.title = Qt.binding(function () { return root.t("gui.choose_config") })
            item.accepted.connect(function () {
                var path = root.bridge.urlToPath(item.selectedFile)
                if (path !== "") {
                    pathField.text = path
                    root.applyPath(path)
                }
            })
        }

        // 只给开发者看，理由同 main.qml 里托盘那条：不走 tr()，必须是纯 ASCII。
        onStatusChanged: {
            if (status === Loader.Error)
                console.warn("ConfigFileDialog.qml failed to load; Browse disabled (QtQuick.Dialogs missing?)")
        }
    }

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

            FieldRow {
                label: ""
                hint: root.bridge && !root.bridge.autostartSupported
                      ? root.t("gui.autostart_unsupported") : root.t("gui.autostart_hint")
                Layout.fillWidth: true
                ToggleBox {
                    id: autostartBox
                    text: root.t("gui.autostart")
                    enabled: root.bridge ? root.bridge.autostartSupported : false
                    Layout.fillWidth: true
                    // 登记失败就把勾退回去：开关显示的应当是系统里实际登记着的状态。
                    // 原因由 bridge 写进顶部的错误提示条。
                    onToggled: {
                        if (root.loading || !root.bridge) return
                        if (!root.bridge.setAutostart(checked))
                            checked = root.bridge.autostartEnabled()
                    }
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
                id: pathRow
                label: root.t("gui.config_path")
                // 出错时把说明让给错误，免得两段小字挤在一起。
                hint: root.pathError === "" ? root.t("gui.config_path_hint") : ""
                Layout.fillWidth: true
                LineInput {
                    id: pathField
                    Layout.fillWidth: true
                    onAccepted: root.applyPath(text)
                    onTextEdited: root.pathError = ""
                }
                ActionButton {
                    text: root.t("gui.browse")
                    // 文件对话框装不上时不显示：手填路径照样能用。
                    visible: dialogLoader.status === Loader.Ready
                    onClicked: {
                        dialogLoader.item.currentFolder = root.bridge.folderUrl(pathField.text)
                        dialogLoader.item.open()
                    }
                }
                ActionButton {
                    text: root.t("gui.apply")
                    variant: "primary"
                    enabled: pathField.text.trim() !== ""
                    onClicked: root.applyPath(pathField.text)
                }
            }

            RowLayout {
                readonly property bool custom: root.bridge !== null
                                               && root.bridge.configPath !== root.bridge.defaultConfigPath
                visible: root.pathError !== "" || custom
                spacing: theme.gap
                Layout.fillWidth: true
                Layout.leftMargin: pathRow.labelWidth + theme.gap

                Text {
                    text: root.pathError
                    color: theme.danger
                    font.pixelSize: theme.fontSmall
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                ActionButton {
                    text: root.t("gui.use_default")
                    compact: true
                    visible: parent.custom
                    onClicked: {
                        pathField.text = root.bridge.defaultConfigPath
                        root.applyPath(pathField.text)
                    }
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

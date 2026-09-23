// 单条规则卡片。
//
// 文案一律经 bridge.tr() 取，不用 qsTr：语言切换要即时生效，而 qsTr 依赖
// QTranslator，重启才会变。状态文案的键是 `state.<status>`，和模型给的
// 状态串同名，省一张映射表。
//
// 悬停才露出编辑与删除：列表通常十来条，常驻两个图标会把卡片压得很满。
import QtQuick 2.15
import QtQuick.Layouts 1.15

Item {
    id: root

    property var bridge: null

    property string ruleId:   ""
    property string ruleName: ""
    property string listen:   ""
    property string detail:   ""
    property string status:   "stopped"   // stopped | starting | running | failed
    property bool   ruleEnabled: true
    property string trafficUp:   ""
    property string trafficDown: ""
    property int    connections: 0
    property int    errors:      0

    signal startRequested()
    signal stopRequested()
    signal editRequested()
    signal deleteRequested()
    signal enabledToggled(bool value)

    Theme { id: theme }

    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }
    function tf(key, args) {
        return bridge ? (bridge.language, bridge.trFmt(key, JSON.stringify(args))) : ""
    }

    implicitHeight: card.implicitHeight

    Rectangle {
        id: card
        anchors.left: parent.left
        anchors.right: parent.right
        implicitHeight: inner.implicitHeight + 20
        radius: theme.radius
        color: hover.hovered ? theme.raised : theme.surface
        border.width: 1
        // 停掉的规则整卡压暗，一眼能从列表里区分出来。
        border.color: root.status === "running" ? Qt.darker(theme.success, 1.8) : theme.border
        // 停用的规则整卡压暗。0.6 会把卡里的小字压到 4:1 以下，
        // 「已停用」看得出来就够了，不必压到看不清。
        opacity: root.ruleEnabled ? 1.0 : 0.75

        Behavior on color { ColorAnimation { duration: 120 } }

        HoverHandler { id: hover }

        ColumnLayout {
            id: inner
            anchors {
                left: parent.left; right: parent.right; top: parent.top
                leftMargin: theme.pad; rightMargin: theme.pad; topMargin: 10
            }
            spacing: 4

            RowLayout {
                Layout.fillWidth: true
                spacing: theme.gap

                ToggleBox {
                    checked: root.ruleEnabled
                    // onToggled 只在用户点击时发，属性回写不会触发，不会成环。
                    onToggled: root.enabledToggled(checked)
                }

                Rectangle {
                    implicitWidth: 8
                    implicitHeight: 8
                    radius: 4
                    color: theme.statusColor(root.status)
                }

                Text {
                    text: root.ruleName.length > 0 ? root.ruleName : root.ruleId
                    font.pixelSize: theme.fontTitle
                    font.weight: Font.Medium
                    color: theme.body
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                Text {
                    text: root.t("state." + root.status)
                    font.pixelSize: theme.fontSmall
                    color: theme.statusColor(root.status)
                }

                ActionButton {
                    text: root.status === "running" ? root.t("gui.stop") : root.t("gui.start")
                    enabled: root.status !== "starting"
                    compact: true
                    // 英文的 "Start"/"Stop" 比中文宽，留够免得被省略号吃掉。
                    implicitWidth: 72
                    onClicked: root.status === "running" ? root.stopRequested()
                                                        : root.startRequested()
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: theme.gap

                Text {
                    text: root.listen
                    font.pixelSize: theme.fontSmall
                    font.family: theme.mono
                    color: theme.accent
                }

                Text {
                    text: "·"
                    color: theme.muted
                    font.pixelSize: theme.fontSmall
                }

                Text {
                    text: root.detail
                    font.pixelSize: theme.fontSmall
                    color: theme.muted
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }
            }

            RowLayout {
                Layout.fillWidth: true
                Layout.topMargin: 2
                spacing: theme.gap

                Text {
                    text: root.tf("gui.traffic", { up: root.trafficUp, down: root.trafficDown })
                    font.pixelSize: theme.fontSmall
                    font.family: theme.mono
                    color: theme.muted
                }

                Text {
                    text: root.tf("gui.connections", { count: root.connections })
                    font.pixelSize: theme.fontSmall
                    color: theme.muted
                }

                Text {
                    visible: root.errors > 0
                    text: root.tf("gui.errors", { count: root.errors })
                    font.pixelSize: theme.fontSmall
                    color: theme.danger
                }

                Item { Layout.fillWidth: true }

                // 悬停才显示，但始终占位：否则卡片里的这一行会随鼠标进出抖一下。
                ActionButton {
                    text: root.t("gui.edit")
                    visible: hover.hovered
                    compact: true
                    onClicked: root.editRequested()
                }

                ActionButton {
                    text: root.t("gui.delete")
                    visible: hover.hovered
                    compact: true
                    labelColor: theme.danger
                    onClicked: root.deleteRequested()
                }
            }
        }
    }
}

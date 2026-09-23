// 日志面板。
//
// 条目由 LogModel 从环形缓冲取，写入侧已经按级别过滤过，这里不再筛。
// 等宽字体让时间戳与级别列对齐，扫读时眼睛不用左右找。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

Item {
    id: root

    property var bridge: null
    property var model: null

    Theme { id: theme }

    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            // 工具条里的按钮长到 28，36 会把它顶满。
            Layout.preferredHeight: 44
            color: theme.surface

            // 只画底边，四面描边在贴着别的面板时会显出双线。
            Rectangle {
                anchors.bottom: parent.bottom
                width: parent.width
                height: 1
                color: theme.border
            }

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: theme.pad
                anchors.rightMargin: theme.gap
                spacing: theme.gap

                Text {
                    text: root.t("gui.logs")
                    font.pixelSize: theme.fontTitle
                    font.weight: Font.Medium
                    // 这是这一屏的标题，用正文色；muted 是给次要信息的。
                    color: theme.body
                }

                Item { Layout.fillWidth: true }

                ToggleBox {
                    id: autoScroll
                    checked: true
                    text: root.t("gui.auto_scroll")
                }

                ActionButton {
                    text: root.t("gui.clear_logs")
                    compact: true
                    onClicked: if (root.model) root.model.clear()
                }
            }
        }

        ListView {
            id: logList
            Layout.fillWidth: true
            Layout.fillHeight: true
            model: root.model
            clip: true
            spacing: 0

            // positionViewAtEnd 要等布局算完才准，延一帧再滚。
            onCountChanged: if (autoScroll.checked) scrollTimer.restart()

            Timer {
                id: scrollTimer
                interval: 0
                onTriggered: logList.positionViewAtEnd()
            }

            ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

            Text {
                anchors.centerIn: parent
                visible: logList.count === 0
                text: root.t("gui.empty_logs")
                color: theme.muted
                font.pixelSize: theme.fontTitle
            }

            delegate: Item {
                width: logList.width
                implicitHeight: row.implicitHeight + 10

                Rectangle {
                    anchors.fill: parent
                    color: hov.hovered ? theme.raised : "transparent"
                    HoverHandler { id: hov }
                }

                RowLayout {
                    id: row
                    anchors {
                        left: parent.left; right: parent.right
                        verticalCenter: parent.verticalCenter
                        leftMargin: theme.pad; rightMargin: theme.pad
                    }
                    spacing: theme.gap

                    Text {
                        text: model.display_timestamp ?? ""
                        font.pixelSize: theme.fontSmall
                        font.family: theme.mono
                        color: theme.muted
                        // "12:34:56" 共 8 个等宽字，12px 下约 58px；留到 70 免得贴边。
                        Layout.preferredWidth: 70
                    }

                    Text {
                        // 颜色由 LogModel 的 level_color 角色给，和级别一一对应。
                        text: model.display_level ?? ""
                        color: model.display_level_color ?? theme.muted
                        font.pixelSize: theme.fontSmall
                        font.family: theme.mono
                        Layout.preferredWidth: 52
                    }

                    Text {
                        visible: (model.display_rule_id ?? "") !== ""
                        text: "[" + (model.display_rule_id ?? "") + "]"
                        font.pixelSize: theme.fontSmall
                        font.family: theme.mono
                        color: theme.accent
                    }

                    Text {
                        text: model.display_message ?? ""
                        font.pixelSize: theme.fontBody
                        color: theme.body
                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                        Layout.fillWidth: true
                    }
                }
            }
        }
    }
}

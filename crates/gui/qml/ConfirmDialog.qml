// 通用确认框。删除规则、退出程序都用它，文案与按钮语义由调用方给。
//
// 不用 Dialog 的 standardButtons：那套按钮走 Basic 样式的浅色外观，
// 且文案取自 Qt 自带翻译，和界面语言对不上。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

Dialog {
    id: root

    property var bridge: null

    property string message: ""
    /// 确认按钮文案。留空则用「确定」，调用方想写「删除」「退出」时覆盖它。
    property string acceptText: t("gui.confirm")
    property string rejectText: t("gui.cancel")
    /// 确认按钮的语义色，默认按「这一步不可逆」处理。
    property string acceptVariant: "danger"

    Theme { id: theme }

    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }

    modal: true
    // 挂到 overlay 上才能盖住整窗并居中，挂在某个面板里会被它的布局裁掉。
    parent: Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(420, Math.max(300, (parent ? parent.width : 420) - 64))
    padding: theme.pad
    closePolicy: Popup.CloseOnEscape

    background: Rectangle {
        color: theme.surface
        radius: theme.radius
        border.width: 1
        border.color: theme.borderStrong
    }

    contentItem: Text {
        text: root.message
        color: theme.body
        font.pixelSize: theme.fontBody
        lineHeight: 1.35
        wrapMode: Text.WordWrap
    }

    // Dialog 的 padding 只作用于 contentItem，页脚得自己留边距。
    footer: Item {
        implicitHeight: theme.controlHeight + theme.pad * 2

        RowLayout {
            anchors.fill: parent
            anchors.margins: theme.pad
            spacing: theme.gap

            Item { Layout.fillWidth: true }

            ActionButton {
                text: root.rejectText
                onClicked: root.reject()
            }

            ActionButton {
                text: root.acceptText
                variant: root.acceptVariant
                onClicked: root.accept()
            }
        }
    }
}

// 复选框。指示器与文字都自绘，原因同 ActionButton。
import QtQuick 2.15
import QtQuick.Controls 2.15

CheckBox {
    id: root

    Theme { id: theme }

    font.pixelSize: theme.fontBody
    spacing: theme.gap
    padding: 0
    hoverEnabled: true

    // 勾选框本身 18×18，但整个 CheckBox 保持 controlHeight 高：
    // 点击判定跟着控件走，指示器小一点不影响好不好点。
    implicitHeight: theme.controlHeight

    indicator: Rectangle {
        implicitWidth: 18
        implicitHeight: 18
        x: root.leftPadding
        y: (root.height - height) / 2
        radius: 3
        // 勾是白的，底色得用暗的那支蓝，否则勾几乎看不出来。
        color: root.checked ? theme.accentFill : theme.raised
        border.width: 1
        border.color: root.checked ? theme.accentFill
                                   : root.hovered ? theme.muted : theme.borderStrong
        opacity: root.enabled ? 1.0 : theme.disabledOpacity

        Text {
            anchors.centerIn: parent
            visible: root.checked
            text: "✓"
            color: theme.onFill
            font.pixelSize: 13
            font.bold: true
        }
    }

    contentItem: Text {
        leftPadding: root.indicator.width + root.spacing
        text: root.text
        color: theme.body
        font: root.font
        opacity: root.enabled ? 1.0 : theme.disabledOpacity
        verticalAlignment: Text.AlignVCenter
        wrapMode: Text.WordWrap
    }
}

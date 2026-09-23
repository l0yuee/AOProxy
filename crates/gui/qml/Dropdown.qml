// 下拉框。model 一律是字符串数组，标签与内部取值的映射由调用方自己维护
// （规则里的 mode/upstream/auth 都是小写枚举，界面上显示的是译文）。
import QtQuick 2.15
import QtQuick.Controls 2.15

ComboBox {
    id: root

    /// 可选的显示文案数组，与 model 等长。model 放不变的内部取值、labels 放译文，
    /// 语言切换时只有 labels 变，currentIndex 不会被 model 变更重置成 0。
    property var labels: null

    Theme { id: theme }

    implicitHeight: theme.controlHeight
    font.pixelSize: theme.fontBody
    displayText: labels && labels[currentIndex] !== undefined ? labels[currentIndex]
                                                             : currentText

    background: Rectangle {
        color: theme.raised
        radius: theme.radius
        border.width: 1
        border.color: root.activeFocus || root.popup.visible ? theme.accent
                                                             : theme.borderStrong
    }

    contentItem: Text {
        leftPadding: 10
        rightPadding: root.indicator.width + 10
        text: root.displayText
        color: theme.body
        font: root.font
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    indicator: Text {
        x: root.width - width - 10
        y: (root.height - height) / 2
        text: "▾"
        color: theme.muted
        font.pixelSize: theme.fontBody
    }

    delegate: ItemDelegate {
        id: item
        width: root.width
        // 和输入框同高：下拉项是要拿鼠标点的，26px 偏窄。
        height: theme.controlHeight
        highlighted: root.highlightedIndex === index
        padding: 0

        background: Rectangle {
            color: item.highlighted ? theme.raised : theme.surface
        }

        contentItem: Text {
            leftPadding: 10
            text: root.labels && root.labels[index] !== undefined ? root.labels[index]
                                                                 : modelData
            color: theme.body
            font.pixelSize: theme.fontBody
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }

    // 弹出层自带的白底会在深色界面上闪一下，连边框一起换掉。
    popup: Popup {
        y: root.height + 2
        width: root.width
        padding: 1
        implicitHeight: Math.min(contentItem.implicitHeight + 2, 260)

        background: Rectangle {
            color: theme.surface
            radius: theme.radius
            border.width: 1
            border.color: theme.borderStrong
        }

        contentItem: ListView {
            clip: true
            implicitHeight: contentHeight
            model: root.popup.visible ? root.delegateModel : null
            currentIndex: root.highlightedIndex
            ScrollIndicator.vertical: ScrollIndicator {}
        }
    }
}

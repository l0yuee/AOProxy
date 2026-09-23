// 深色主题下的按钮。
//
// Basic 样式的默认外观是浅色系的，background 与 contentItem 必须整个换掉，
// 只改 palette 不够。三种语义：normal 描边、primary 与 danger 填色。
// 一屏里通常只让一个动作填色，其余描边，视觉重量才有区分。
//
// 填色用 theme 的 *Fill 一组而不是同名的亮色：白字压在亮蓝上只有 3.2:1，
// 压在 accentFill 上有 4.65:1。描边用 borderStrong，因为描边按钮底色透明，
// 全靠那条线表明自己可点。
import QtQuick 2.15
import QtQuick.Controls 2.15

Button {
    id: root

    /// "normal" | "primary" | "danger"
    property string variant: "normal"

    /// 紧凑尺寸，用于卡片内与工具条。仍有 28px 高，满足最小可点区域。
    /// 调用方别再自己写 implicitHeight：那正是之前按钮缩到 22px 的来路。
    property bool compact: false

    /// 描边按钮的文字色。想要一个「危险但不抢眼」的按钮时改这里，
    /// 而不是把 variant 设成 danger 让它整块变红。
    property color labelColor: theme.body

    readonly property bool filled: variant === "primary" || variant === "danger"
    readonly property color fill: variant === "danger" ? theme.dangerFill : theme.accentFill

    Theme { id: theme }

    implicitHeight: compact ? theme.controlHeightSmall : theme.controlHeight
    // 横向留白跟着高度走，免得紧凑按钮显得又矮又宽。
    leftPadding: compact ? 12 : 16
    rightPadding: compact ? 12 : 16
    topPadding: 0
    bottomPadding: 0
    hoverEnabled: true
    font.pixelSize: compact ? theme.fontSmall : theme.fontBody

    background: Rectangle {
        radius: theme.radius
        color: root.filled
               ? (root.down ? Qt.darker(root.fill, 1.25)
                            : root.hovered ? Qt.lighter(root.fill, 1.12)
                                           : root.fill)
               : (root.down ? theme.border
                            : root.hovered ? theme.raised
                                           : "transparent")
        border.width: root.filled ? 0 : 1
        border.color: theme.borderStrong
        opacity: root.enabled ? 1.0 : theme.disabledOpacity
    }

    contentItem: Text {
        text: root.text
        color: root.filled ? theme.onFill : root.labelColor
        font: root.font
        opacity: root.enabled ? 1.0 : theme.disabledOpacity
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }
}

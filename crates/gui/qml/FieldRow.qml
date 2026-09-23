// 表单里的一行：左标签 + 右控件（可放多个，横向排开）+ 可选说明。
//
// 默认属性指向右侧的 RowLayout，于是写法是
//
//     FieldRow { label: "端口"; LineInput { Layout.fillWidth: true } }
//
// 标签宽度固定，多行表单的控件左缘才对得齐。
import QtQuick 2.15
import QtQuick.Layouts 1.15

ColumnLayout {
    id: root

    property string label: ""
    /// 灰色小字说明，空串则整行不占高度。
    property string hint: ""
    /// 中文标签四五个字就到头，英文的 "Upstream username" 要宽一些；
    /// 字号调大之后 104 会把英文挤成两行。
    property int labelWidth: 124

    // 默认属性被改指到 slot，Theme 只能挂成具名属性——写成子元素会被塞进 slot。
    property Theme theme: Theme {}

    default property alias content: slot.data

    spacing: 2

    RowLayout {
        Layout.fillWidth: true
        spacing: root.theme.gap

        Text {
            text: root.label
            // 标签用正文色而不是 muted：它说明右边那个框要填什么，
            // 是表单里第一眼要读的东西，不该比输入的内容还淡。
            color: root.theme.body
            font.pixelSize: root.theme.fontBody
            // 控件高 32，标签顶对齐后往下垫一点才和输入框文字同高。
            topPadding: 8
            wrapMode: Text.WordWrap
            Layout.minimumWidth: root.labelWidth
            Layout.preferredWidth: root.labelWidth
            Layout.alignment: Qt.AlignTop
        }

        RowLayout {
            id: slot
            spacing: root.theme.gap
            Layout.fillWidth: true
        }
    }

    Text {
        visible: root.hint !== ""
        text: root.hint
        color: root.theme.muted
        font.pixelSize: root.theme.fontSmall
        wrapMode: Text.WordWrap
        Layout.leftMargin: root.labelWidth + root.theme.gap
        Layout.fillWidth: true
    }
}

// 单行输入框。
//
// 名字不叫 TextInput：那是 QtQuick 的内置类型，同名文件会把它遮住。
import QtQuick 2.15
import QtQuick.Controls 2.15

TextField {
    id: root

    Theme { id: theme }

    implicitHeight: theme.controlHeight
    leftPadding: 10
    rightPadding: 10
    topPadding: 0
    bottomPadding: 0
    color: theme.body
    font.pixelSize: theme.fontBody
    placeholderTextColor: theme.muted
    selectByMouse: true
    // 选中底色用填充色那一组：亮蓝配白字只有 3.2:1，选中的那段反而看不清。
    selectionColor: theme.accentFill
    selectedTextColor: theme.onFill

    background: Rectangle {
        color: theme.raised
        radius: theme.radius
        border.width: 1
        // 聚焦时描边点亮：深色底上光标本身不够醒目。
        border.color: root.activeFocus ? theme.accent : theme.borderStrong
    }
}

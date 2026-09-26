// 选择配置文件的对话框。
//
// 单独成文件、由设置页经 Loader 装载，原因与托盘一样：它依赖 QtQuick.Dialogs，
// 这个模块缺席时（Linux 上是没装 qml6-module-qtquick-dialogs）直接写进设置页
// 会让整个主窗口加载失败。装不上只是少一个「浏览…」按钮，手填路径照样能用。
//
// 标题、初始目录与选中后的处理都由设置页负责，这里只定对话框的行为。
import QtQuick 2.15
import QtQuick.Dialogs 6.2

FileDialog {
    // 既能选已有的文件，也能填一个新文件名：已有的会被载入，新的则把当前配置存过去。
    // 所以用保存模式，并且不问「是否覆盖」——选中已有文件是载入它，不会覆盖。
    fileMode: FileDialog.SaveFile
    options: FileDialog.DontConfirmOverwrite
    defaultSuffix: "toml"
    nameFilters: ["TOML (*.toml)", "* (*)"]
}

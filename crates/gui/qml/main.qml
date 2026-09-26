// 主窗口。
//
// 三个桥接对象在这里各实例化一份；它们背后共享同一个引擎与日志缓冲
// （见 bridge::shared），所以谁先谁后都不影响。
//
// 界面文案全部经 bridge.tr() / trFmt()，且每个取词函数都先读一下
// bridge.language——QML 不会因为函数体里调了 invokable 就重新求值，
// 必须引用一个会变的属性，语言切换才能刷到已经画好的绑定上。
// Window.Minimized 这类枚举不必另导 QtQuick.Window：Qt 6 起 Window 类型已并入
// QtQuick，那个模块的 qmldir 只剩一句 `import QtQuick auto` 转发。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15
import AOProxy 1.0

ApplicationWindow {
    id: root

    width: 940
    height: 640
    minimumWidth: 720
    minimumHeight: 480
    // 先不显示，由 initialShow() 决定：开机自启时窗口应当待在托盘里，
    // 先显示再隐藏会在登录时闪一下。
    visible: false
    // 标题带版本号：任务栏悬停与 alt-tab 都只看得到这一行，
    // 出问题时让人报版本比让人去翻「关于」省事。
    title: t("app.name") + " " + bridge.version
    color: theme.bg

    Theme { id: theme }

    /// 当前页：0 规则 / 1 日志 / 2 设置
    property int tab: 0

    function t(key) { return bridge.language, bridge.tr(key) }
    function tf(key, args) { return bridge.language, bridge.trFmt(key, JSON.stringify(args)) }

    AppBridge {
        id: bridge

        // 规则的运行态变了就让模型重取一遍：流量与连接数也在同一份快照里。
        onStatusChanged: ruleModel.refresh()
        onLanguageChanged: ruleModel.refresh()
        // 换了配置文件，规则整批换了。设置页自己也接这个信号重读各项。
        onConfigChanged: ruleModel.refresh()
        onShowRequested: root.wakeUp()
        // stop_all 收尾完才到这里。先放行关闭再请求退出：Qt 6 的 quit() 会先逐个
        // close 顶层窗口，任何一个拒绝就整个作废。下面的 onClosing 若照旧拦下，
        // 托盘模式下表现为窗口缩回托盘、进程还在，于是退出要一点再点。
        onQuitRequested: {
            root.readyToQuit = true
            Qt.quit()
        }
    }

    RuleModel {
        id: ruleModel
        Component.onCompleted: refresh()
    }

    LogModel { id: logModel }

    // 托盘经 Loader 装载，不直接实例化。
    //
    // Tray.qml 依赖 Qt.labs.platform。这个模块缺席时（典型是 windeployqt 漏了
    // --qmldir，它只扫二进制里的 C++ 依赖，扫不出 QML 侧的 import；Linux 上也
    // 可能没装）直接写 `Tray {}` 会让整个 main.qml 连带加载失败——release 是
    // windows 子系统没有控制台，表现就是进程起来了却什么都不显示，且无处报错。
    // 装进 Loader 后这类失败降级成 status === Loader.Error：窗口照常出现，
    // trayAvailable 置为 false，关窗按退出处理，只是没有托盘。
    //
    // 用 setSource 而不是 source 属性：bridge 必须在组件完成之前就位，
    // Tray.qml 的 Component.onCompleted 要靠它回填 trayAvailable。
    Loader {
        id: trayLoader
        Component.onCompleted: setSource("Tray.qml", { "bridge": bridge })

        onLoaded: {
            item.showWindowRequested.connect(root.wakeUp)
            item.quitRequested.connect(root.requestQuit)
        }

        // 这条只给开发者看（release 没有控制台），所以不走 tr()，也必须是纯 ASCII：
        // qmlcachegen 把 QML 里的字面量原样写进 UTF-8 的 .cpp，而 cl.exe 按本机代码页
        // （中文 Windows 是 936）读它，多字节序列会把收尾的引号吃掉 —— 报 C2001 加 C1057。
        // QML 里所有给用户看的文案都经 bridge.tr()，中文只留在注释里。
        //
        // trayAvailable 的初值只是按平台猜的（Windows 上恒为 true），托盘装不上时必须
        // 在这里改掉，否则关窗照样缩进一个并不存在的托盘，窗口从此叫不回来。
        onStatusChanged: {
            if (status === Loader.Error) {
                console.warn("Tray.qml failed to load; tray disabled (Qt.labs.platform missing?)")
                bridge.trayAvailable = false
            }
        }
    }

    // 定时刷新流量计数。运行态变化有信号可接，但字节数是连接里累加的，
    // 没有「变了」的通知；两秒一次足够看出趋势，也不会把 CPU 烧在重绘上。
    Timer {
        interval: 2000
        running: bridge.runningCount > 0 && root.visible && root.tab === 0
        repeat: true
        onTriggered: ruleModel.refresh()
    }

    // 退出已在进行中。stop_all 要等各条规则收尾，期间界面还活着，
    // 不挡住就会重复触发 bridge.quit()。
    property bool quitting: false

    // 引擎已停妥、可以真正退出。Qt 6 的 quit() 靠逐个 close 顶层窗口来收尾，
    // onClosing 只有见到这个标记才放行关闭，否则一律拦下走隐藏或确认流程。
    property bool readyToQuit: false

    /// 把窗口叫到前台。托盘点击、再次启动本程序（单实例守卫拦下的那次）都走这里。
    ///
    /// 已最小化时不能只 show()：那只管 visible，windowState 还是 Minimized，
    /// 窗口依旧待在任务栏里——看起来就像「又启动了一次但什么都没发生」。
    /// 所以先把 visibility 拨回 Windowed，最大化状态则原样保留。
    function wakeUp() {
        if (root.visibility === Window.Minimized)
            root.visibility = Window.Windowed
        root.show()
        root.raise()
        root.requestActivate()
    }

    /// 关窗是否只是缩到托盘。没有托盘时这个开关不起作用——否则窗口一关就
    /// 再也叫不回来了。
    function wantsTray() {
        if (!bridge.trayAvailable)
            return false
        try {
            return JSON.parse(bridge.appConfigJson()).minimize_to_tray !== false
        } catch (e) {
            return false
        }
    }

    /// 退出入口。有规则在跑就先问一声，没有就直接走。
    ///
    /// 问之前必须先把窗口叫到前台：托盘菜单的退出可以在窗口缩进托盘、最小化
    /// 或被别的窗口压住时触发，而确认框是挂在本窗口 overlay 上的模态框，窗口
    /// 看不见它也看不见——表现就是点了退出毫无反应，于是一按再按。最小化的窗口
    /// visible 仍为 true，所以不看 visible，一律唤起；已在前台时 wakeUp 等于空操作。
    function requestQuit() {
        if (root.quitting)
            return
        if (bridge.runningCount > 0) {
            root.wakeUp()
            quitDialog.open()
        } else {
            root.quitting = true
            bridge.quit()
        }
    }

    // 关闭事件有两种来路，得分开处理：
    //
    // - readyToQuit 已置位：这是 quitRequested→Qt.quit() 主动发来的 close。
    //   Qt 6 的 quit() 靠逐个 close 顶层窗口收尾，这里必须放行，否则退出作废，
    //   进程留在后台——就是「退出要点好几次」的根因。
    // - 否则是用户点了窗口的关闭按钮：拦下，缩到托盘（有托盘且开了该选项）或
    //   转入 requestQuit（无托盘）。readyToQuit 要等引擎停妥、onQuitRequested
    //   执行时才置位；确认框被取消就一直不置位，窗口照常留着。
    onClosing: function (event) {
        if (root.readyToQuit)
            return
        event.accepted = false
        if (root.wantsTray())
            root.hide()
        else
            root.requestQuit()
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // ── 顶栏 ─────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            // 按钮长到 32 高之后 52 只剩 10px 余量，顶栏会显得被塞满。
            Layout.preferredHeight: 60
            color: theme.surface

            Rectangle {
                anchors.bottom: parent.bottom
                width: parent.width
                height: 1
                color: theme.border
            }

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: theme.pad
                anchors.rightMargin: theme.pad
                spacing: theme.gap

                Text {
                    text: root.t("app.name")
                    font.pixelSize: theme.fontLarge
                    font.weight: Font.DemiBold
                    color: theme.body
                }

                // 版本徽标。做成带底色的小块而不是跟在名字后面的灰字：
                // 顶栏这一行还有运行计数，两段小字并排容易读成一句话。
                Rectangle {
                    Layout.alignment: Qt.AlignVCenter
                    implicitWidth: versionLabel.implicitWidth + 12
                    implicitHeight: 20
                    radius: 10
                    color: theme.raised
                    border.width: 1
                    border.color: theme.border

                    Text {
                        id: versionLabel
                        anchors.centerIn: parent
                        text: "v" + bridge.version
                        font.pixelSize: theme.fontSmall
                        font.family: theme.mono
                        color: theme.muted
                    }
                }

                Text {
                    text: root.tf("gui.running_count",
                                  { count: bridge.runningCount, total: ruleModel.count })
                    font.pixelSize: theme.fontSmall
                    color: bridge.runningCount > 0 ? theme.success : theme.muted
                }

                Item { Layout.fillWidth: true }

                ActionButton {
                    text: root.t("gui.new_rule")
                    variant: "primary"
                    onClicked: editor.openNew()
                }

                ActionButton {
                    text: root.t("gui.start_all")
                    onClicked: bridge.startAll()
                }

                ActionButton {
                    text: root.t("gui.stop_all")
                    onClicked: bridge.stopAll()
                }
            }
        }

        // ── 页签 ─────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            // 页签整块都是点击区，40 高让它好点，也给放大后的字留出余量。
            Layout.preferredHeight: 40
            color: theme.bg

            Row {
                anchors.fill: parent
                anchors.leftMargin: theme.pad
                spacing: 0

                Repeater {
                    model: [root.t("gui.rules"), root.t("gui.logs"), root.t("gui.settings")]

                    Item {
                        required property int index
                        required property string modelData

                        readonly property bool current: root.tab === index

                        width: label.implicitWidth + 36
                        height: parent.height

                        Text {
                            id: label
                            anchors.centerIn: parent
                            text: parent.modelData
                            font.pixelSize: theme.fontBody
                            // 选中项加粗：只靠颜色区分对色觉障碍不友好，
                            // 下面那条下划线也是同一个道理。
                            font.weight: parent.current ? Font.DemiBold : Font.Normal
                            color: parent.current ? theme.body
                                                  : tapHover.hovered ? theme.body : theme.muted
                        }

                        // 选中态用下划线而不是整块底色：页签条与内容区同底色，
                        // 填色块会把两者切开，反而更碎。
                        Rectangle {
                            anchors.bottom: parent.bottom
                            width: parent.width
                            height: 2
                            color: parent.current ? theme.accent : "transparent"
                        }

                        HoverHandler { id: tapHover }
                        TapHandler { onTapped: root.tab = parent.index }
                    }
                }
            }

            Rectangle {
                anchors.bottom: parent.bottom
                width: parent.width
                height: 1
                color: theme.border
                z: -1
            }
        }

        // ── 错误提示条 ───────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: visible ? 38 : 0
            visible: bridge.lastError !== ""
            // 比正文底色暗的红：错误文字本身是亮红，底色再亮两者就糊在一起。
            color: Qt.darker(theme.danger, 3.4)

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: theme.pad
                anchors.rightMargin: theme.gap
                spacing: theme.gap

                Text {
                    text: bridge.lastError
                    // 错误条底色已经是暗红，文字用接近白的正文色最清楚；
                    // 红底红字即便比值够也难读。
                    color: theme.body
                    font.pixelSize: theme.fontBody
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                ActionButton {
                    text: "✕"
                    // 原来 24×22，是全界面最难点的一个。见方 28 才好瞄。
                    implicitHeight: theme.iconButton
                    implicitWidth: theme.iconButton
                    leftPadding: 0
                    rightPadding: 0
                    labelColor: theme.body
                    onClicked: bridge.clearError()
                }
            }
        }

        // ── 内容区 ───────────────────────────────────────────
        StackLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: root.tab

            // 规则列表
            Item {
                ListView {
                    id: ruleList
                    anchors.fill: parent
                    anchors.margins: theme.pad
                    model: ruleModel
                    spacing: theme.gap
                    clip: true

                    ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

                    // 给委托用的 AppBridge 引用，原因见下面 `bridge:` 那行。
                    readonly property var appBridge: bridge

                    delegate: RuleCard {
                        width: ruleList.width - (ruleList.ScrollBar.vertical.visible ? 12 : 0)
                        // 这里不能照别处写 `bridge: bridge`。委托比其他面板多隔一层上下文，
                        // 右边这个不带限定的名字会先命中 RuleCard 自身的 bridge 属性，
                        // 成了自己绑自己，值始终为空；卡片里的 t() 因此全部返回空串——
                        // 状态文字、流量行，以及启动/停止、编辑、删除按钮上的字都显示不出来，
                        // 只剩按钮边框。其他面板和 bridge 这个 id 在同一层上下文里，
                        // id 先于属性被找到，所以直接写没问题。
                        // 下面几个信号处理里的 bridge 也是卡片自身的这个属性，赋值后即为 AppBridge。
                        bridge: ruleList.appBridge

                        ruleId:      model.display_id ?? ""
                        ruleName:    model.display_name ?? ""
                        listen:      model.display_listen ?? ""
                        detail:      model.display_detail ?? ""
                        status:      model.display_status ?? "stopped"
                        ruleEnabled: model.display_enabled ?? true
                        trafficUp:   model.display_bytes_up ?? "0 B"
                        trafficDown: model.display_bytes_down ?? "0 B"
                        connections: model.display_connections ?? 0
                        errors:      model.display_errors ?? 0

                        onStartRequested: bridge.startRule(ruleId)
                        onStopRequested: bridge.stopRule(ruleId)
                        onEnabledToggled: function (value) { bridge.setRuleEnabled(ruleId, value) }
                        onEditRequested: editor.openExisting(ruleId)
                        onDeleteRequested: deleteDialog.ask(ruleId, ruleName || ruleId)
                    }

                    Text {
                        anchors.centerIn: parent
                        visible: ruleList.count === 0
                        text: root.t("gui.empty_rules")
                        color: theme.muted
                        font.pixelSize: theme.fontTitle
                    }
                }
            }

            LogView {
                bridge: bridge
                model: logModel
            }

            SettingsPanel { bridge: bridge }
        }
    }

    RuleEditor {
        id: editor
        bridge: bridge
        onSaved: ruleModel.refresh()
    }

    ConfirmDialog {
        id: deleteDialog
        bridge: bridge
        acceptText: root.t("gui.delete")

        property string pendingId: ""

        function ask(id, name) {
            pendingId = id
            message = root.tf("gui.delete_confirm", { name: name })
            open()
        }

        onAccepted: {
            bridge.deleteRule(pendingId)
            ruleModel.refresh()
        }
    }

    ConfirmDialog {
        id: quitDialog
        bridge: bridge
        message: root.t("gui.quit_confirm")
        acceptText: root.t("gui.quit")
        onAccepted: {
            root.quitting = true
            bridge.quit()
        }
    }

    /// 首次露面。开机自启拉起来时待在托盘里；没有托盘（或关掉了缩到托盘）就最小化到
    /// 任务栏——总之别在登录时把窗口甩到用户面前。平常启动照常显示。
    function initialShow() {
        if (!bridge.launchedAtLogin)
            root.show()
        else if (!root.wantsTray())
            root.showMinimized()
    }

    Component.onCompleted: {
        // 托盘缺失只提醒一次，且只在「关闭即缩到托盘」真的失效时才有意义。
        if (!bridge.trayAvailable)
            console.warn(root.t("gui.tray_missing"))
        // 托盘到底能不能用，要等 Tray.qml 装完回填 trayAvailable 才知道，而各个
        // onCompleted 的先后没有保证：推到本轮事件处理的末尾再决定显示与否。
        Qt.callLater(root.initialShow)
        // 开机自启的意义在于登录后代理就在跑，不必再手动点「全部启用」。
        if (bridge.launchedAtLogin)
            bridge.startAll()
    }
}

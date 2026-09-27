// 规则编辑对话框。新建与修改是同一条路径：`ruleJson("")` 给模板，
// `ruleJson(id)` 给现有规则，表单只负责把 JSON 铺进控件、再拼回 JSON。
//
// 字段可见性跟着模式与认证方式走：正向模式没有目标地址，路径令牌只在反向
// 模式下成立。入站 TLS 两种模式都有：正向模式配上 TLS 就是 HTTPS 代理，
// 公网网关正是这么用的（见 README 的服务器示例）。
//
// 校验分两道：界面先查几项能当场说清楚的（端口没填、证书只给了一半），
// 再经 bridge.checkRule 跑核心的完整校验——与保存时引擎做的是同一套，
// 只是提前到对话框关闭之前，错误就显示在对话框里。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

Dialog {
    id: root

    property var bridge: null
    /// 空串表示新建。非空时 ID 不可改，免得改完变成新增一条、旧的还留着。
    /// 新建时 ID 不得与已有规则重复，否则按 ID 覆盖会把那条规则悄悄换掉。
    property string editingId: ""
    /// 界面侧校验的结果，存不下去时显示在页脚。
    property string localError: ""

    /// 存盘成功。调用方据此让列表重取——新增的那条要立刻出现在界面上。
    signal saved()

    Theme { id: theme }

    // tr() 是函数，绑定不会因语言切换自动失效；先读一次 language 建立依赖。
    function t(key) { return bridge ? (bridge.language, bridge.tr(key)) : "" }

    readonly property var modeTags: ["forward", "reverse"]
    readonly property var upstreamTags: ["direct", "http", "https", "socks5"]
    readonly property var authTags: ["none", "basic", "token"]

    readonly property string mode: modeTags[modeBox.currentIndex]
    readonly property string upstreamKind: upstreamTags[upstreamBox.currentIndex]
    readonly property string authKind: authTags[authBox.currentIndex]
    readonly property bool reverse: mode === "reverse"

    /// 把一条规则读进表单并打开对话框。id 为空取新建模板。
    function load(id) {
        editingId = id
        localError = ""
        var r = JSON.parse(bridge.ruleJson(id))
        nameField.text = r.name || ""
        idField.text = id
        enabledBox.checked = r.enabled === true
        modeBox.currentIndex = Math.max(0, modeTags.indexOf(r.mode || "reverse"))
        // listen 是 "host:port"，从最后一个冒号切，IPv6 的 "[::1]:8080" 也对。
        var listen = r.listen || ""
        var cut = listen.lastIndexOf(":")
        hostField.text = cut < 0 ? listen : listen.substring(0, cut)
        portField.text = cut < 0 ? "" : listen.substring(cut + 1)
        targetField.text = r.target || ""
        var up = r.upstream || {}
        upstreamBox.currentIndex = Math.max(0, upstreamTags.indexOf(up.kind || "direct"))
        upAddr.text = up.address || ""
        upUser.text = up.username || ""
        upPass.text = up.password || ""
        var auth = r.auth || {}
        authBox.currentIndex = Math.max(0, authTags.indexOf(auth.kind || "none"))
        authUser.text = auth.username || ""
        authPass.text = auth.password || ""
        tokenField.text = auth.token || ""
        var tls = r.tls || {}
        certField.text = tls.cert || ""
        keyField.text = tls.key || ""
        open()
    }

    function openNew() { load("") }
    function openExisting(id) { load(id) }

    /// 由名称推导 ID：只留字母数字与 -_，其余并成连字符。
    /// 中文名什么都留不下，退回时间戳——总比和别的规则撞 ID 好。
    function deriveId(name) {
        var s = name.toLowerCase().replace(/[^a-z0-9_-]+/g, "-")
                    .replace(/-+/g, "-").replace(/^-|-$/g, "")
        return s.length > 0 ? s : "rule-" + Date.now().toString(36)
    }

    /// 收表单。可选字段一律「空就不写」：core 那边是 deny_unknown_fields
    /// 配 skip_serializing_if，塞 null 进去会直接解析失败。
    function collect() {
        var id = idField.text.trim()
        var r = {
            id: id !== "" ? id : deriveId(nameField.text),
            name: nameField.text.trim(),
            enabled: enabledBox.checked,
            mode: mode,
            listen: hostField.text.trim() + ":" + portField.text.trim(),
            upstream: { kind: upstreamKind },
            auth: { kind: authKind }
        }
        if (reverse && targetField.text.trim() !== "")
            r.target = targetField.text.trim()
        if (upstreamKind !== "direct") {
            if (upAddr.text.trim() !== "") r.upstream.address = upAddr.text.trim()
            if (upUser.text !== "") r.upstream.username = upUser.text
            if (upPass.text !== "") r.upstream.password = upPass.text
        }
        if (authKind === "basic") {
            if (authUser.text !== "") r.auth.username = authUser.text
            if (authPass.text !== "") r.auth.password = authPass.text
        } else if (authKind === "token" && tokenField.text !== "") {
            r.auth.token = tokenField.text
        }
        if (certField.text.trim() !== "" || keyField.text.trim() !== "")
            r.tls = { cert: certField.text.trim(), key: keyField.text.trim() }
        return r
    }

    /// 保存前校验，返回空串表示可以存。前面几项是界面能当场说清楚的；最后交给
    /// checkRule 跑核心的完整校验（ID 字符集与是否重复、端口冲突、证书与私钥是否
    /// 配对……），与 `aoproxy config check` 结论一致。ID 的字符集只在核心那边查：
    /// 那里认 Unicode 字母，界面若只认 ASCII，配置文件里的中文 ID 规则就改不了了。
    function checkLocal(r) {
        if (r.id === "") return t("valid.id_empty")
        if (hostField.text.trim() === "" || !/^[0-9]+$/.test(portField.text.trim()))
            return t("valid.listen_invalid")
        if (reverse) {
            if (!r.target) return t("valid.target_required")
            if (r.target.indexOf("http://") !== 0 && r.target.indexOf("https://") !== 0)
                return t("valid.target_scheme")
        }
        if (upstreamKind !== "direct" && !r.upstream.address)
            return t("valid.upstream_address")
        if ((r.upstream.username === undefined) !== (r.upstream.password === undefined))
            return t("valid.upstream_auth")
        if (authKind === "basic" && (!r.auth.username || !r.auth.password))
            return t("valid.auth_basic_incomplete")
        if (authKind === "token") {
            if (!reverse) return t("valid.auth_token_forward")
            if (!r.auth.token) return t("valid.auth_token_empty")
        }
        if (r.tls && (r.tls.cert === "" || r.tls.key === ""))
            return t("valid.tls_incomplete")
        return bridge.checkRule(JSON.stringify(r), editingId === "")
    }

    function submit() {
        var r = collect()
        var err = checkLocal(r)
        if (err !== "") {
            localError = err
            return
        }
        localError = ""
        bridge.clearError()
        bridge.saveRule(JSON.stringify(r), editingId === "")
        saved()
        close()
    }

    modal: true
    parent: Overlay.overlay
    anchors.centerIn: parent
    // 比初版宽高各多 60：字号与控件都长了一圈，原尺寸下表单会一直要滚。
    width: Math.min(620, (parent ? parent.width : 620) - 40)
    height: Math.min(660, (parent ? parent.height : 660) - 40)
    padding: theme.pad
    closePolicy: Popup.CloseOnEscape

    background: Rectangle {
        color: theme.surface
        radius: theme.radius
        border.width: 1
        border.color: theme.borderStrong
    }

    // 校验失败的原因就摆在按钮旁边：表单可以滚动，把错误放顶部的话
    // 用户点「保存」时未必看得见。
    footer: Item {
        implicitHeight: theme.controlHeight + theme.pad * 2

        RowLayout {
            anchors.fill: parent
            anchors.margins: theme.pad
            spacing: theme.gap

            Text {
                text: root.localError
                color: theme.danger
                font.pixelSize: theme.fontBody
                wrapMode: Text.WordWrap
                maximumLineCount: 2
                elide: Text.ElideRight
                Layout.fillWidth: true
            }

            ActionButton {
                text: root.t("gui.cancel")
                onClicked: root.close()
            }

            ActionButton {
                text: root.t("gui.save")
                variant: "primary"
                onClicked: root.submit()
            }
        }
    }

    header: Item {
        implicitHeight: 52

        Text {
            anchors.fill: parent
            anchors.leftMargin: theme.pad
            anchors.rightMargin: theme.pad
            text: root.editingId === "" ? root.t("gui.new_rule_title")
                                        : root.t("gui.edit_rule")
            color: theme.body
            font.pixelSize: theme.fontLarge
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }

    contentItem: ScrollView {
        id: scroll
        clip: true
        contentWidth: availableWidth

        ColumnLayout {
            width: scroll.availableWidth
            spacing: theme.gap

            FieldRow {
                label: root.t("gui.name")
                Layout.fillWidth: true
                LineInput { id: nameField; Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.rule_id")
                // 提示只在新建时有意义：改 ID 等于换一条规则，编辑时锁住。
                hint: root.editingId === "" ? root.t("gui.rule_id_hint") : ""
                Layout.fillWidth: true
                LineInput {
                    id: idField
                    readOnly: root.editingId !== ""
                    opacity: readOnly ? 0.6 : 1.0
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.mode")
                Layout.fillWidth: true
                Dropdown {
                    id: modeBox
                    model: root.modeTags
                    labels: [root.t("mode.forward"), root.t("mode.reverse")]
                    Layout.preferredWidth: 170
                }
                ToggleBox {
                    id: enabledBox
                    text: root.t("gui.enabled")
                    Layout.leftMargin: theme.gap
                }
                Item { Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.listen_host")
                Layout.fillWidth: true
                LineInput {
                    id: hostField
                    placeholderText: "127.0.0.1"
                    Layout.fillWidth: true
                }
                Text {
                    text: root.t("gui.listen_port")
                    // 和 FieldRow 的标签同一个角色，配色也该一致。
                    color: theme.body
                    font.pixelSize: theme.fontBody
                    verticalAlignment: Text.AlignVCenter
                    Layout.leftMargin: theme.gap
                }
                LineInput {
                    id: portField
                    // 端口只收数字：把「不合法」挡在输入阶段，省一次校验往返。
                    validator: IntValidator { bottom: 1; top: 65535 }
                    inputMethodHints: Qt.ImhDigitsOnly
                    // 五位端口号在 13px 下约 40px，加上左右各 10 的内边距取 84。
                    Layout.preferredWidth: 84
                }
            }

            FieldRow {
                label: root.t("gui.target")
                visible: root.reverse
                Layout.fillWidth: true
                LineInput {
                    id: targetField
                    placeholderText: "https://api.example.com"
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.upstream_kind")
                Layout.fillWidth: true
                Dropdown {
                    id: upstreamBox
                    model: root.upstreamTags
                    labels: [root.t("upstream.direct"), root.t("upstream.http"),
                             root.t("upstream.https"), root.t("upstream.socks5")]
                    Layout.preferredWidth: 170
                }
                Item { Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.upstream_address")
                visible: root.upstreamKind !== "direct"
                Layout.fillWidth: true
                LineInput {
                    id: upAddr
                    placeholderText: "127.0.0.1:7890"
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.upstream_username")
                hint: root.t("gui.optional")
                visible: root.upstreamKind !== "direct"
                Layout.fillWidth: true
                LineInput { id: upUser; Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.upstream_password")
                visible: root.upstreamKind !== "direct"
                Layout.fillWidth: true
                LineInput {
                    id: upPass
                    echoMode: TextInput.Password
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.auth_kind")
                Layout.fillWidth: true
                Dropdown {
                    id: authBox
                    model: root.authTags
                    labels: [root.t("auth.none"), root.t("auth.basic"), root.t("auth.token")]
                    Layout.preferredWidth: 170
                }
                Item { Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.auth_username")
                visible: root.authKind === "basic"
                Layout.fillWidth: true
                LineInput { id: authUser; Layout.fillWidth: true }
            }

            FieldRow {
                label: root.t("gui.auth_password")
                visible: root.authKind === "basic"
                Layout.fillWidth: true
                LineInput {
                    id: authPass
                    echoMode: TextInput.Password
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.auth_token")
                visible: root.authKind === "token"
                Layout.fillWidth: true
                LineInput {
                    id: tokenField
                    echoMode: TextInput.Password
                    Layout.fillWidth: true
                }
            }

            // 入站 TLS，两种模式都适用：反向模式是 HTTPS 服务端，正向模式是 HTTPS 代理。
            // 早先这一段只在反向模式下显示，收集时也只收反向模式的——结果是在界面上
            // 随便改一下正向规则（比如只改个名字），配置文件里的 TLS 就被悄悄删掉了。
            Text {
                text: root.t("gui.tls_section")
                // 分节标题，用正文色加粗；muted 小字在这里会被当成脚注忽略掉。
                color: theme.body
                font.pixelSize: theme.fontBody
                font.weight: Font.DemiBold
                Layout.fillWidth: true
                Layout.topMargin: theme.gap
            }

            Rectangle {
                implicitHeight: 1
                color: theme.border
                Layout.fillWidth: true
                Layout.bottomMargin: theme.gap / 2
            }

            FieldRow {
                label: root.t("gui.tls_cert")
                hint: root.t("gui.optional")
                Layout.fillWidth: true
                LineInput {
                    id: certField
                    placeholderText: "C:\\certs\\site.pem"
                    Layout.fillWidth: true
                }
            }

            FieldRow {
                label: root.t("gui.tls_key")
                Layout.fillWidth: true
                LineInput {
                    id: keyField
                    placeholderText: "C:\\certs\\site.key"
                    Layout.fillWidth: true
                }
            }
        }
    }

}

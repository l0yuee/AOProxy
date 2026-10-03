import QtQuick 2.15
import QtTest 1.2
import "../qml" as App

TestCase {
    id: testCase
    name: "Tray"

    property var tray: null
    property var bridge: null

    Component {
        id: bridgeComponent
        QtObject {
            property string language: "en-US"
            // The real Rust translation methods have no QML property dependencies.
            // Mutating this object's field likewise emits no change notification.
            property var translations: ({ language: "en-US" })
            property int runningCount: 0
            property bool trayAvailable: false
            property bool loggingEnabled: false
            property string logLevel: "info"
            property var rules: [
                { id: "a", name: "Rule A", enabled: true, status: "stopped" },
                { id: "b", name: "Rule B", enabled: false, status: "stopped" }
            ]
            property var requests: []
            signal statusChanged()
            signal configChanged()
            function tr(key) { return translations.language + ":" + key }
            function trFmt(key, args) { return translations.language + ":" + key }
            function ruleMenuJson() { return JSON.stringify(rules) }
            function setRuleEnabled(id, enabled) { requests.push({ id: id, enabled: enabled }) }
            function enableLogging(enabled) { loggingEnabled = enabled }
            function selectLogLevel(level) { logLevel = level }
        }
    }

    Component {
        id: trayComponent
        App.Tray { visible: false }
    }

    function init() {
        bridge = createTemporaryObject(bridgeComponent, testCase)
        tray = createTemporaryObject(trayComponent, testCase, { bridge: bridge })
        verify(tray !== null)
    }

    function ruleItem(index) { return tray.menu.items[2].subMenu.items[index] }

    function activate(item) {
        // Run the native MenuItem activation slot, including its automatic toggle.
        verify(nativeMenuTest.activate(item))
    }

    function test_initial_checks() {
        compare(ruleItem(0).checked, true)
        compare(ruleItem(1).checked, false)
    }

    function test_toggle_roundtrip() {
        activate(ruleItem(0))
        compare(bridge.requests.length, 1)
        compare(bridge.requests[0].enabled, false)
        bridge.rules[0].enabled = false
        bridge.statusChanged()
        compare(ruleItem(0).checked, false)
        activate(ruleItem(0))
        compare(bridge.requests[1].enabled, true)
        bridge.rules[0].enabled = true
        bridge.statusChanged()
        compare(ruleItem(0).checked, true)
    }

    function test_rejected_toggle_restores_check() {
        activate(ruleItem(0))
        bridge.statusChanged()
        compare(ruleItem(0).checked, true)
    }

    function test_external_change() {
        bridge.rules[0].enabled = false
        bridge.rules[1].enabled = true
        bridge.statusChanged()
        compare(ruleItem(0).checked, false)
        compare(ruleItem(1).checked, true)
    }

    function test_running_tooltip_language_changes() {
        bridge.runningCount = 1
        compare(tray.tooltip, "en-US:gui.tray_tip_running")
        bridge.translations.language = "zh-CN"
        bridge.language = "zh-CN"
        compare(tray.tooltip, "zh-CN:gui.tray_tip_running")
    }

    function test_config_replaced() {
        bridge.rules = [{ id: "c", name: "A&B", enabled: false, status: "failed" }]
        bridge.configChanged()
        compare(ruleItem(0).checked, false)
        verify(ruleItem(0).text.indexOf("A&&B") === 0)
        activate(ruleItem(0))
        compare(bridge.requests[0].id, "c")
        compare(bridge.requests[0].enabled, true)
    }

    function test_runtime_status_does_not_change_enabled_preference() {
        bridge.rules[0].status = "failed"
        bridge.statusChanged()
        compare(ruleItem(0).checked, true)
        verify(ruleItem(0).text.indexOf("state.failed") !== -1)
        bridge.rules[0].status = "starting"
        bridge.statusChanged()
        compare(ruleItem(0).enabled, false)
    }
}

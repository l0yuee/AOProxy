// 配色与尺寸令牌。
//
// 普通 QtObject 而非 `pragma Singleton`：单例要靠 qmldir 登记，而 qmldir 由
// cxx-qt-build 生成，插不进去。各文件自己 `Theme { id: theme }` 实例化一份，
// 代价是一行，换来令牌只此一处定义。
//
// 导入 QtQuick 而不是更轻的 QtQml：`color` 是 QtQuick 带来的值类型，
// 只导 QtQml 会在运行期报 "color is not a type"——这类错误编译期查不出来。
//
// 配色按 WCAG 2.1 的对比度门槛选：正文与小字对各自底色不低于 4.5:1，
// 控件描边不低于 3:1。深色界面上「好看的灰」通常只有 3:1 出头，看着还行，
// 实际在笔记本屏幕或阳光下就糊了——所以这里的值是算出来的，不是挑出来的。
// 改动任何一个色值都请重新核对下面注释里标的比值。
import QtQuick 2.15

QtObject {
    // ── 表面 ──────────────────────────────────────────────
    readonly property color bg:       "#18191c"   // 窗口底
    readonly property color surface:  "#222428"   // 卡片、顶栏
    readonly property color raised:   "#2b2d32"   // 悬停、输入框

    // 分隔线。纯装饰，不承担「这里是个控件」的信息，所以不要求 3:1。
    readonly property color border:   "#3f4149"

    // 控件描边（按钮、输入框、下拉框、复选框）。对 raised 3.07:1、
    // 对 surface 3.46:1，满足 WCAG 1.4.11 对图形界面元素的 3:1 要求。
    // 描边按钮底色透明，全靠这条线表明自己可点，不能再暗。
    readonly property color borderStrong: "#727782"

    // ── 文字 ──────────────────────────────────────────────
    // body  对 surface 12.69:1，对 raised 11.25:1
    // muted 对 surface  5.98:1，对 raised  5.30:1（原 #767a84 只有 3.62:1）
    readonly property color body:     "#e6e8ee"
    readonly property color muted:    "#9aa1ad"

    // ── 语义色（用作文字与细线）────────────────────────────
    // 都对 raised ≥ 4.74:1。这几个值偏亮，是因为深底上的彩色文字
    // 想同时「够鲜艳」和「够清楚」就只能往亮处走。
    readonly property color accent:   "#7aa5f8"   // raised 5.62:1
    readonly property color success:  "#5fc596"   // raised 6.50:1
    readonly property color warning:  "#f5b731"   // raised 7.68:1
    readonly property color danger:   "#ef7070"   // raised 4.74:1

    // ── 语义色（用作填充底色）──────────────────────────────
    // 填色按钮的文字是白的，底色越亮文字越糊：上面那组亮色配白字只有 2~3:1。
    // 所以填充另取一组暗的，白字压上去 ≥ 4.65:1。
    // 一句话：文字用亮的那组，底色用暗的这组，别混。
    readonly property color accentFill:  "#2f6fe4"   // 白字 4.65:1
    readonly property color dangerFill:  "#c62f2f"   // 白字 5.46:1
    readonly property color successFill: "#27805c"   // 白字 4.85:1
    readonly property color onFill:      "#ffffff"

    // ── 排版 ──────────────────────────────────────────────
    // 整体比初版大 1~2px：11px 的中文在 100% 缩放的 Windows 上笔画会粘连，
    // 而界面里大量信息（流量、连接数、日志时间戳）恰恰都用的最小号。
    readonly property int fontSmall:  12
    readonly property int fontBody:   13
    readonly property int fontTitle:  15
    readonly property int fontLarge:  18

    // 等宽族按平台给候选：Qt 会挑第一个装得上的。
    // 写死 "monospace" 在 Windows 上会落到点阵字体，太糊。
    readonly property string mono: "Consolas, Menlo, DejaVu Sans Mono, monospace"

    // ── 间距 ──────────────────────────────────────────────
    readonly property int gap:        8
    readonly property int pad:        12
    readonly property int radius:     6

    // ── 控件尺寸 ──────────────────────────────────────────
    // WCAG 2.2 的 2.5.8 要求可点区域不小于 24×24；这里取 32 作常规、
    // 28 作紧凑（卡片内与工具条），图标按钮 28×28 见方。
    // 原先卡片里的按钮只有 22~24 高、图标按钮 24×22，鼠标得瞄准了点。
    readonly property int controlHeight:      32
    readonly property int controlHeightSmall: 28
    readonly property int iconButton:         28

    // 禁用态的淡化程度。0.45 会把正文压到 3.7:1，虽然 WCAG 对禁用控件不作要求，
    // 但「看不清」和「不可用」是两件事，0.6 已经足够表达后者。
    readonly property real disabledOpacity: 0.6

    // 规则状态 → 颜色。停止态用 muted 而非灰字，
    // 免得「已停止」看着像禁用。
    function statusColor(s) {
        switch (s) {
        case "running":  return success
        case "starting": return warning
        case "failed":   return danger
        default:         return muted
        }
    }
}

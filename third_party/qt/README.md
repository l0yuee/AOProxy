# 随附的 Qt

GUI（`aoproxy-gui`）依赖 Qt 6。为了让项目拷到任意一台 Windows 机器上只装 Rust 就能完整构建，
这里放了一份裁剪过的 Qt 官方二进制。

| 项 | 值 |
|---|---|
| 版本 | 6.8.3 |
| 构建 | `msvc2022_64`（Windows x86-64，MSVC 2022，Release） |
| 来源 | Qt 官方在线安装器 |

`.cargo/config.toml` 把 `QMAKE` 指向 `third_party/qt/bin/qmake.exe`，用的是相对项目根的路径。
`bin/qt.conf` 里写的是 `Prefix=..`，Qt 自身也只按 qmake 所在位置解析前缀，因此整个目录可以随项目
任意搬动，无需重新配置。

只有 Windows 构建用得上这里的文件。在 Linux / macOS 上构建 GUI 需要系统自带的 Qt 6，并在构建时
用环境变量 `QMAKE` 指向它的 qmake——外部设置的值优先于 `.cargo/config.toml`。

## 裁剪了什么

官方安装结果约 1.9 GB，这里保留 198 MB。去掉的是 Windows 上构建与运行本项目用不到的部分：

- Debug 版的库（`*d.dll`、`*d.lib`、`*d.prl`）。Rust 在 MSVC 上始终链接 Release 版 CRT，
  `cargo build` 与 `cargo build --release` 用的都是不带 `d` 后缀的那套。
- 全部调试符号与中间产物（`*.pdb`、`*.obj`）。
- 文档、翻译短语本、SBOM 清单，以及 `qml` 下的示例资源。
- `bin` 下除构建与部署工具外的其他可执行文件。保留的是：
  `qmake`、`moc`、`rcc`、`qmltyperegistrar`、`qmlcachegen`、`qmlimportscanner`、`qmllint`、
  `qtpaths`、`qtpaths6`、`windeployqt`、`lconvert`。
  其中 `lconvert` 是 `windeployqt` 生成 `translations\qt_*.qm` 时调用的，少了它部署会中途失败
  （报 `CreateProcessW failed`）——除非机器上另有 Qt 或 Anaconda 把同名程序放进了 `PATH`，
  那样就只是恰好借到了别处的副本。
- `lib` 下没有对应 DLL 的导入库。

头文件、mkspecs、metatypes、插件、QML 模块、翻译文件（`.qm`）和全部 Release DLL 均完整保留，
构建和 `windeployqt` 部署都不需要再补文件。

## 许可证

AOProxy 本身是 MIT。Qt 不是——这里随附的 Qt 二进制按其开源许可证（LGPL-3.0，部分组件 GPL-3.0）分发，
许可证全文在 `LICENSES/` 下。以 LGPL 条款分发本项目的二进制时，需要满足该许可证对动态链接、
相应源码获取途径和替换 Qt 库的可能性等方面的要求。详见 [Qt Licensing](https://www.qt.io/licensing/)。

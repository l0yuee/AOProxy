use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    stage_tray_icons();

    // QML 模块的 bridge 文件只在 rust_files 中登记；再用 .file() 重复添加会生成两份 C++ 代码。
    CxxQtBuilder::new()
        .qml_module(QmlModule {
            uri: "AOProxy",
            version_major: 1,
            version_minor: 0,
            qml_files: &[
                "qml/main.qml",
                "qml/Theme.qml",
                "qml/RuleCard.qml",
                "qml/RuleEditor.qml",
                "qml/SettingsPanel.qml",
                "qml/LogView.qml",
                "qml/Tray.qml",
                "qml/ConfirmDialog.qml",
                // 依赖 QtQuick.Dialogs，由设置页经 Loader 装载，缺这个模块时只少一个按钮。
                "qml/ConfigFileDialog.qml",
                "qml/FieldRow.qml",
                // 控件基元：Basic 样式的默认外观是浅色的，按钮、输入框、
                // 复选框、下拉框都得整块换掉，换一次这里存一份。
                "qml/ActionButton.qml",
                "qml/LineInput.qml",
                "qml/ToggleBox.qml",
                "qml/Dropdown.qml",
            ],
            rust_files: &[
                "src/bridge/app.rs",
                "src/bridge/rule_model.rs",
                "src/bridge/log_model.rs",
            ],
            // 托盘图标必须进 qrc：release 是 windows 子系统，工作目录不确定，
            // 相对路径取不到文件。别名就是这里写的相对路径，于是运行时路径为
            // `qrc:/qt/qml/AOProxy/qml/icons/tray-idle.ico`。
            qrc_files: &[
                "qml/icons/tray-idle.ico",
                "qml/icons/tray-active.ico",
            ],
            ..Default::default()
        })
        .build();

    embed_windows_icon();
    deploy_qt();
}

/// 把 Qt 运行库部署到 `target/<profile>`，让编出来的 exe 双击即可运行。
///
/// 目标是「编完就能跑」：按手册编译之后，target 下的 exe 不该再要求使用者
/// 装一份 Qt 或手动拷 DLL。这件事交给 windeployqt 而不是自己抄一份清单——
/// 它知道该带哪些插件（platforms\qwindows.dll 一类，缺了就是「起了进程但
/// 没有窗口」）与 QML 模块，清单手抄迟早会漏。
///
/// 有一处限制绕不开：build.rs 在链接之前运行，干净构建时 `aoproxy-gui.exe`
/// 还不存在，没法照常把它交给 windeployqt 扫依赖。于是拿 Qt 自带的
/// `qmlimportscanner.exe` 当扫描对象：它链的是同一套 Qt6Core/Gui/Qml，
/// 扫出来的 C++ 依赖是本程序所需的超集；QML 那一侧本来就不看二进制，由
/// `--qmldir` 扫源码得到。代价是多带几个用不上的样式模块，换来的是
/// 不依赖链接顺序，`cargo build` 一条命令就位。
fn deploy_qt() {
    use std::path::{Path, PathBuf};

    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    // 打包脚本若要自行部署（比如换一套精简过的 Qt），用这个开关跳过。
    println!("cargo:rerun-if-env-changed=AOPROXY_NO_QT_DEPLOY");
    if std::env::var_os("AOPROXY_NO_QT_DEPLOY").is_some() {
        return;
    }

    let Some(qt_bin) = qt_bin_dir() else {
        println!("cargo:warning=未能由 QMAKE 定位 Qt 的 bin 目录，target 下不会有 Qt 运行库");
        return;
    };
    let windeployqt = qt_bin.join("windeployqt.exe");
    let scanner = qt_bin.join("qmlimportscanner.exe");
    if !windeployqt.is_file() || !scanner.is_file() {
        println!(
            "cargo:warning={} 下缺少 windeployqt.exe 或 qmlimportscanner.exe，跳过 Qt 部署",
            qt_bin.display()
        );
        return;
    }

    // OUT_DIR 形如 target/<profile>/build/<crate>-<hash>/out，
    // 上溯三级正是 exe 的落地目录。指定 --target 时多一层三元组，
    // 相对层级不变。
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(dest) = out.ancestors().nth(3).map(Path::to_path_buf) else {
        println!("cargo:warning=无法由 OUT_DIR 推出 target 目录，跳过 Qt 部署");
        return;
    };

    let qml_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let stamp_path = dest.join(".aoproxy-qt-deploy");
    let stamp = deploy_stamp(&qt_bin, &windeployqt, &qml_dir);
    // 部署一次上千个文件，每次构建都重来会把增量编译的好处吃光。
    // 标记文件相同且关键文件还在，就认为上一次的部署仍然有效。
    let current = std::fs::read_to_string(&stamp_path).unwrap_or_default();
    if current == stamp && qt_runtime_present(&dest) {
        return;
    }

    let result = std::process::Command::new(&windeployqt)
        // 随项目的 Qt 只有 release 一套库，debug 构建也链它，所以固定按
        // release 部署；否则 windeployqt 会去找不存在的 Qt6Cored.dll。
        .arg("--release")
        .arg("--no-translations")
        .arg("--no-opengl-sw")
        .arg("--no-system-d3d-compiler")
        .arg("--no-system-dxc-compiler")
        // 部署到 exe 所在目录，而不是扫描对象（qmlimportscanner）旁边。
        .arg("--dir")
        .arg(&dest)
        // 必须给：windeployqt 只扫二进制里的 C++ 依赖，扫不出 QML 侧的
        // import。漏了它，Qt.labs.platform 一类模块不会被带上，界面能起来
        // 但托盘没了。
        .arg("--qmldir")
        .arg(&qml_dir)
        .arg(&scanner)
        .output();

    match result {
        Ok(o) if o.status.success() => {
            let _ = std::fs::write(&stamp_path, stamp);
        }
        Ok(o) => {
            // 末尾几行通常就是原因（找不到 qmldir、目标目录不可写之类）。
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: Vec<&str> = err.lines().rev().take(3).collect();
            println!(
                "cargo:warning=windeployqt 失败（{}）：{}",
                o.status,
                tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
            );
        }
        Err(e) => println!("cargo:warning=无法执行 windeployqt: {e}"),
    }
}

/// 由 `QMAKE` 推出 Qt 的 bin 目录。构建 Qt 相关的一切都靠这一个变量定位，
/// 见 `.cargo/config.toml`。
fn qt_bin_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    println!("cargo:rerun-if-env-changed=QMAKE");
    let qmake = PathBuf::from(std::env::var_os("QMAKE")?);
    // config.toml 里写的是相对仓库根的路径，cargo 以 relative = true 解析成
    // 绝对路径后才传进来；万一是别处设的相对值，按仓库根再补一次。
    let qmake = if qmake.is_absolute() {
        qmake
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(qmake)
    };
    let dir = qmake.parent()?.to_path_buf();
    dir.is_dir().then_some(dir)
}

/// 部署是否还在。三个哨兵分别代表核心库、平台插件与 QML 模块，
/// 少哪一类都会以不同的方式起不来，所以各查一个。
fn qt_runtime_present(dir: &std::path::Path) -> bool {
    dir.join("Qt6Core.dll").is_file()
        && dir.join("platforms").join("qwindows.dll").is_file()
        && dir.join("qml").join("QtQuick").join("qmldir").is_file()
}

/// 部署的输入指纹。换一套 Qt、或 QML 里多了一句 import，都要重新部署；
/// 只改界面代码则不必——后者每次构建都会让 build.rs 重跑（qml 文件在
/// rerun-if-changed 里），指纹不变就直接跳过。
///
/// 存明文而不是哈希：出问题时 `type .aoproxy-qt-deploy` 一眼能看出
/// 是哪一项变了。
fn deploy_stamp(
    qt_bin: &std::path::Path,
    windeployqt: &std::path::Path,
    qml_dir: &std::path::Path,
) -> String {
    let mut stamp = String::from("v1\n");
    stamp.push_str(&format!("qt={}\n", qt_bin.display()));
    if let Ok(meta) = std::fs::metadata(windeployqt) {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        stamp.push_str(&format!("windeployqt={}:{mtime}\n", meta.len()));
    }

    let mut imports: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(qml_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("qml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("import ") {
                    imports.push(rest.trim().to_owned());
                }
            }
        }
    }
    imports.sort();
    imports.dedup();
    stamp.push_str("imports=");
    stamp.push_str(&imports.join(" | "));
    stamp.push('\n');
    stamp
}

/// 把托盘图标从 `assets/icons/` 拷进 `qml/icons/`。
///
/// `qrc_files` 的路径必须在 crate 目录之内（别名由相对路径生成），而图标的源头在
/// 仓库根的 `assets/`——那里由 `tools/gen-icons.py` 统一生成，不该有第二份手抄。
/// 于是构建时拷一次：源文件一改，`rerun-if-changed` 会让它重新拷。
fn stage_tray_icons() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dest_dir = root.join("qml/icons");
    if std::fs::create_dir_all(&dest_dir).is_err() {
        println!("cargo:warning=无法创建 qml/icons，托盘图标将缺失");
        return;
    }
    for name in ["tray-idle.ico", "tray-active.ico"] {
        let src = root.join("../../assets/icons").join(name);
        println!("cargo:rerun-if-changed=../../assets/icons/{name}");
        if !src.exists() {
            println!("cargo:warning=未找到 {}", src.display());
            continue;
        }
        // 内容相同就不写：避免每次构建都改文件时间戳，把 qrc 重新编一遍。
        let dest = dest_dir.join(name);
        let fresh = match (std::fs::read(&src), std::fs::read(&dest)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !fresh {
            if let Err(e) = std::fs::copy(&src, &dest) {
                println!("cargo:warning=拷贝 {name} 失败: {e}");
            }
        }
    }
}

/// 把应用图标编进 exe 的资源段。
///
/// cxx-qt-lib 0.7 没有 `QIcon` / `QGuiApplication::setWindowIcon` 绑定，
/// 任务栏与 alt-tab 的图标只能走 Win32 资源这条路：`rc.exe` 编出 `.res`，
/// 交给链接器。找不到 `rc.exe` 时只告警不失败——图标缺失不该挡住构建。
fn embed_windows_icon() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/icons/aoproxy.ico");
    if !ico.exists() {
        println!("cargo:warning=未找到 {}，exe 将不带图标", ico.display());
        return;
    }
    println!("cargo:rerun-if-changed=../../assets/icons/aoproxy.ico");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_path = out.join("app.rc");
    let res_path = out.join("app.res");
    // 同一个 .ico 挂两份，因为取图标的两方按不同的键去找：
    //
    // - 数字 id `1`：Explorer 与任务栏取编号最小的那个当程序图标。
    // - 名字 `IDI_ICON1`：Qt 的 windows 平台插件按这个名字 LoadIcon，
    //   拿不到就让窗口左上角空着（qwindows.dll 里能搜到这个宽字符串，
    //   Qt6Gui.dll 里没有——找窗口图标的是插件，不是 Gui 本体）。
    //   cxx-qt-lib 0.7 没有 QIcon/setWindowIcon 绑定，只能走资源这条路。
    let path = ico.display().to_string().replace('\\', "\\\\");
    let rc = format!("1 ICON \"{path}\"\nIDI_ICON1 ICON \"{path}\"\n");
    if std::fs::write(&rc_path, rc).is_err() {
        return;
    }

    let rc_exe = find_rc_exe();
    let status = std::process::Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bin=aoproxy-gui={}", res_path.display());
        }
        // 带上实际调用的路径：找错了 rc.exe 和根本没找到，排查方向完全不同。
        _ => println!(
            "cargo:warning=rc.exe 执行失败（{}），exe 将不带图标",
            rc_exe.to_string_lossy()
        ),
    }
}

/// 定位 `rc.exe`，都找不到时退回裸名字交给 PATH，失败在上层当告警处理。
///
/// 不能只指望 PATH：Windows SDK 的 bin 目录只有进了 vcvars 环境才在 PATH 上。
/// 在别的终端里构建时这一步会失败，而它只告警不报错——构建照样成功，exe 却
/// 悄悄没了图标，窗口左上角也就跟着没有。所以依次尝试：
///
/// 1. `AOPROXY_RC`：手动指定，覆盖一切（SDK 不在默认位置时用它）。
/// 2. `WindowsSdkVerBinPath`：vcvars 选定的那个 SDK 版本。
/// 3. 默认安装位置 `Windows Kits\10\bin` 下版本号最大的那个。
fn find_rc_exe() -> std::ffi::OsString {
    println!("cargo:rerun-if-env-changed=AOPROXY_RC");
    std::env::var_os("AOPROXY_RC")
        .or_else(|| sdk_rc_exe().map(std::path::PathBuf::into_os_string))
        .unwrap_or_else(|| "rc.exe".into())
}

fn sdk_rc_exe() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    // 构建脚本编给宿主机跑，这里的 target_arch 就是宿主架构，
    // 正对应 SDK bin 目录下按宿主架构分的子目录。
    let host = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    };

    if let Some(bin) = std::env::var_os("WindowsSdkVerBinPath") {
        let rc = PathBuf::from(bin).join(host).join("rc.exe");
        if rc.is_file() {
            return Some(rc);
        }
    }

    let root = std::env::var_os("ProgramFiles(x86)")
        .unwrap_or_else(|| r"C:\Program Files (x86)".into());
    std::fs::read_dir(PathBuf::from(root).join(r"Windows Kits\10\bin"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            // 版本目录形如 `10.0.26100.0`，拆成数字比较而不是按字符串比；
            // 旧版 SDK 留下的 `x64` 这类非版本目录在这一步被滤掉。
            let version = entry
                .file_name()
                .to_str()?
                .split('.')
                .map(|n| n.parse::<u32>().ok())
                .collect::<Option<Vec<_>>>()?;
            let rc = entry.path().join(host).join("rc.exe");
            rc.is_file().then_some((version, rc))
        })
        .max()
        .map(|(_, rc)| rc)
}

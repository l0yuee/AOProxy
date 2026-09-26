//! 开机自启：登录系统后自动启动 GUI。
//!
//! 只有 GUI 用得上，放在 core 是为了能写单元测试——GUI crate 关掉了测试目标
//! （链出来的测试程序要 Qt 的运行库）。这里只和操作系统打交道，与 Qt 无关：
//!
//! | 平台 | 登记在哪 |
//! |---|---|
//! | Windows | 注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下名为 `AOProxy` 的值 |
//! | Linux 等 | XDG 自启目录下的 `aoproxy.desktop`（通常是 `~/.config/autostart/`） |
//! | macOS | `~/Library/LaunchAgents/com.aoproxy.gui.plist` |
//!
//! 开关状态不存进配置文件，每次都去看系统里登记着什么：配置文件可以在几台机器、
//! 几个账户之间拷来拷去，自启却是这台机器这个账户上的事；也只有这样，用户在任务
//! 管理器或桌面环境的「启动应用程序」里关掉自启后，界面上的开关才会跟着变。
//!
//! 登记的命令是「当前这个可执行文件 + [`LAUNCH_FLAG`]」，并且只有登记的正是当前这个
//! 可执行文件时才算已开启：程序挪过位置，旧登记已经失效，界面上理应显示为关。

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 自启时附加的命令行参数。GUI 见到它就缩在托盘里启动，并运行已启用的规则。
pub const LAUNCH_FLAG: &str = "--autostart";

/// 本平台是否支持开机自启。
pub fn is_supported() -> bool {
    imp::SUPPORTED
}

/// 当前这个可执行文件是否已登记为开机自启。
pub fn is_enabled() -> Result<bool> {
    imp::is_enabled(&current_exe()?).map_err(Error::Autostart)
}

/// 登记或撤销开机自启。登记的是当前这个可执行文件。
pub fn set_enabled(enabled: bool) -> Result<()> {
    imp::set_enabled(&current_exe()?, enabled).map_err(Error::Autostart)
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(Error::Autostart)
}

#[cfg(any(test, not(windows)))]
fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.to_owned())
}

// ─────────────── Windows：注册表 Run 键 ───────────────

/// 登记进 Run 键的命令行。路径加引号：`C:\Program Files\…` 这类路径里有空格，
/// 不加的话系统会在第一个空格处截断。Windows 的路径里不会出现引号，不必转义。
#[cfg(any(test, windows))]
fn run_command(exe: &Path) -> String {
    format!("\"{}\" {LAUNCH_FLAG}", exe.display())
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::path::Path;
    use std::ptr;

    pub(super) const SUPPORTED: bool = true;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// 任务管理器「启动」页的开关记在这里，而不是去删 Run 键里的值。
    const APPROVED_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const VALUE_NAME: &str = "AOProxy";

    // advapi32 的几个函数，签名照 Windows SDK。只用这三个，
    // 为此引入 windows-sys 不值当。
    type Hkey = *mut c_void;
    /// SDK 里写作 `(HKEY)(ULONG_PTR)((LONG)0x80000001)`：先按有符号 32 位解释，
    /// 64 位下符号扩展成 0xFFFFFFFF80000001。直接写 `0x80000001 as usize` 是错的。
    const HKEY_CURRENT_USER: Hkey = 0x8000_0001_u32 as i32 as isize as Hkey;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_MORE_DATA: i32 = 234;
    const REG_SZ: u32 = 1;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const RRF_RT_REG_BINARY: u32 = 0x0000_0008;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            hkey: Hkey,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            value_type: *mut u32,
            data: *mut c_void,
            data_len: *mut u32,
        ) -> i32;
        /// 子键不存在时会先创建。
        fn RegSetKeyValueW(
            hkey: Hkey,
            sub_key: *const u16,
            value: *const u16,
            value_type: u32,
            data: *const c_void,
            data_len: u32,
        ) -> i32;
        fn RegDeleteKeyValueW(hkey: Hkey, sub_key: *const u16, value: *const u16) -> i32;
    }

    /// 以 NUL 结尾的 UTF-16，Win32 的 `LPCWSTR`。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// 读 `HKCU\sub_key` 下一个值的原始字节。键或值不存在时返回 `None`。
    fn get_value(sub_key: &str, name: &str, flags: u32) -> io::Result<Option<Vec<u8>>> {
        let sub_key = wide(sub_key);
        let name = wide(name);
        // 先问长度再读。两次调用之间值可能被别人改长（ERROR_MORE_DATA），重试几次。
        let mut len = 0u32;
        for _ in 0..4 {
            let mut buf = vec![0u8; len as usize];
            let mut size = len;
            let data = if buf.is_empty() {
                ptr::null_mut()
            } else {
                buf.as_mut_ptr().cast()
            };
            // SAFETY: 两个字符串都以 NUL 结尾且在调用期间存活；`data` 为空指针（只问长度）
            // 或指向 `size` 字节的可写缓冲。
            let status = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    sub_key.as_ptr(),
                    name.as_ptr(),
                    flags,
                    ptr::null_mut(),
                    data,
                    &mut size,
                )
            };
            match status {
                ERROR_SUCCESS if buf.is_empty() && size > 0 => len = size,
                ERROR_SUCCESS => {
                    buf.truncate(size as usize);
                    return Ok(Some(buf));
                }
                ERROR_MORE_DATA => len = size,
                ERROR_FILE_NOT_FOUND => return Ok(None),
                code => return Err(io::Error::from_raw_os_error(code)),
            }
        }
        Err(io::Error::from_raw_os_error(ERROR_MORE_DATA))
    }

    fn get_string(sub_key: &str, name: &str) -> io::Result<Option<String>> {
        Ok(get_value(sub_key, name, RRF_RT_REG_SZ)?.map(|bytes| {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16_lossy(&units)
        }))
    }

    fn set_string(sub_key: &str, name: &str, value: &str) -> io::Result<()> {
        let sub_key = wide(sub_key);
        let name = wide(name);
        let data = wide(value);
        // SAFETY: 字符串都以 NUL 结尾；长度按字节计，含结尾的 NUL，正是 REG_SZ 的要求。
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                sub_key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(()),
            code => Err(io::Error::from_raw_os_error(code)),
        }
    }

    /// 删掉一个值。本就不存在不算错。
    fn delete_value(sub_key: &str, name: &str) -> io::Result<()> {
        let sub_key = wide(sub_key);
        let name = wide(name);
        // SAFETY: 两个字符串都以 NUL 结尾且在调用期间存活。
        let status =
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, sub_key.as_ptr(), name.as_ptr()) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            code => Err(io::Error::from_raw_os_error(code)),
        }
    }

    pub(super) fn is_enabled(exe: &Path) -> io::Result<bool> {
        let Some(command) = get_string(RUN_KEY, VALUE_NAME)? else {
            return Ok(false);
        };
        // 路径不区分大小写：从快捷方式启动时，盘符与目录名的大小写可能与登记时不同。
        if command.to_lowercase() != super::run_command(exe).to_lowercase() {
            return Ok(false);
        }
        // 在任务管理器里被禁用的自启项，Run 键里的值还在，禁用标记记在
        // StartupApproved 下：首字节为奇数（通常是 0x03）即禁用。读不出来就当没禁用。
        let disabled = get_value(APPROVED_KEY, VALUE_NAME, RRF_RT_REG_BINARY)
            .ok()
            .flatten()
            .and_then(|flags| flags.first().copied())
            .is_some_and(|flag| flag & 1 == 1);
        Ok(!disabled)
    }

    pub(super) fn set_enabled(exe: &Path, enabled: bool) -> io::Result<()> {
        if enabled {
            set_string(RUN_KEY, VALUE_NAME, &super::run_command(exe))?;
        } else {
            delete_value(RUN_KEY, VALUE_NAME)?;
        }
        // 清掉任务管理器留下的禁用标记，否则曾在那边禁用过的话，这边开了也不生效。
        let _ = delete_value(APPROVED_KEY, VALUE_NAME);
        Ok(())
    }
}

// ─────────────── macOS：LaunchAgent ───────────────

#[cfg(any(test, target_os = "macos"))]
const LAUNCHD_LABEL: &str = "com.aoproxy.gui";

/// LaunchAgent 的 plist。`ProgramArguments` 逐个列参数，不经 shell，路径只需做 XML 转义。
#[cfg(any(test, target_os = "macos"))]
fn launch_agent_plist(exe: &Path) -> std::io::Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| invalid("executable path is not valid UTF-8"))?;
    let exe = exe
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>{LAUNCH_FLAG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#
    ))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    pub(super) const SUPPORTED: bool = true;

    fn agent_path() -> io::Result<PathBuf> {
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
        Ok(dirs
            .home_dir()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", super::LAUNCHD_LABEL)))
    }

    /// 文件是本程序整份生成的，内容一致即说明登记的就是当前这个可执行文件。
    pub(super) fn is_enabled(exe: &Path) -> io::Result<bool> {
        match std::fs::read_to_string(agent_path()?) {
            Ok(content) => Ok(content == super::launch_agent_plist(exe)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(super) fn set_enabled(exe: &Path, enabled: bool) -> io::Result<()> {
        super::write_or_remove(
            &agent_path()?,
            enabled.then(|| super::launch_agent_plist(exe)),
        )
    }
}

// ─────────────── Linux 等：XDG 自启目录 ───────────────

/// 自启项文件。`Exec` 行见 [`desktop_exec`]。
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn desktop_entry(exe: &Path) -> std::io::Result<String> {
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=AOProxy\n\
         Comment=AI Agent proxy forwarder\n\
         Exec={}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        desktop_exec(exe)?
    ))
}

/// `Exec` 键的值：程序路径整体加引号，后接 [`LAUNCH_FLAG`]。
///
/// 转义照《桌面项规范》：引号里的 `"`、`` ` ``、`$`、`\` 前面各加一个反斜杠；这层转义
/// 之后还要再过一遍字符串值的转义（反斜杠写成两个），所以路径里的一个反斜杠最终是
/// 四个。`%` 是字段代码的前缀，字面的百分号写成 `%%`。
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn desktop_exec(exe: &Path) -> std::io::Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| invalid("executable path is not valid UTF-8"))?;
    // 换行之类的控制字符在按行组织的文件里写不进去，这样的路径也不会是正经安装位置。
    if exe.chars().any(char::is_control) {
        return Err(invalid("executable path contains control characters"));
    }
    let mut quoted = String::from("\"");
    for c in exe.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str(r"\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str(r"\\\\"),
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    Ok(format!("{quoted} {LAUNCH_FLAG}"))
}

/// 自启项是否对这个可执行文件生效：`Exec` 与本程序会写的一致，且没有被标记为停用。
///
/// GNOME 的「启动应用程序」里关掉一项时不删文件，而是写 `X-GNOME-Autostart-enabled=false`；
/// 规范里的 `Hidden=true` 同样表示停用。
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn desktop_entry_enabled(content: &str, exe: &Path) -> bool {
    let Ok(expected) = desktop_exec(exe) else {
        return false;
    };
    let mut in_entry = false;
    let mut exec_matches = false;
    let mut disabled = false;
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_entry) else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Exec", exec) => exec_matches = exec == expected,
            ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => disabled = true,
            _ => {}
        }
    }
    exec_matches && !disabled
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    pub(super) const SUPPORTED: bool = true;

    /// `$XDG_CONFIG_HOME/autostart/aoproxy.desktop`，变量没设时是 `~/.config/autostart/`。
    fn entry_path() -> io::Result<PathBuf> {
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
        Ok(dirs.config_dir().join("autostart").join("aoproxy.desktop"))
    }

    pub(super) fn is_enabled(exe: &Path) -> io::Result<bool> {
        match std::fs::read_to_string(entry_path()?) {
            Ok(content) => Ok(super::desktop_entry_enabled(&content, exe)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(super) fn set_enabled(exe: &Path, enabled: bool) -> io::Result<()> {
        super::write_or_remove(&entry_path()?, enabled.then(|| super::desktop_entry(exe)))
    }
}

/// `content` 为 `Some` 时写入（目录不存在就建），为 `None` 时删掉，本就不存在不算错。
#[cfg(unix)]
fn write_or_remove(path: &Path, content: Option<std::io::Result<String>>) -> std::io::Result<()> {
    match content {
        Some(content) => {
            let content = content?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, content)
        }
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

// ─────────────── 其他平台 ───────────────

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) const SUPPORTED: bool = false;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "launch at login is not supported on this platform",
        )
    }

    pub(super) fn is_enabled(_exe: &Path) -> io::Result<bool> {
        Ok(false)
    }

    pub(super) fn set_enabled(_exe: &Path, _enabled: bool) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_quotes_the_path() {
        let exe = Path::new(r"C:\Program Files\AOProxy\aoproxy-gui.exe");
        assert_eq!(
            run_command(exe),
            r#""C:\Program Files\AOProxy\aoproxy-gui.exe" --autostart"#
        );
    }

    #[test]
    fn desktop_exec_plain_path() {
        let exe = Path::new("/opt/aoproxy/aoproxy-gui");
        assert_eq!(
            desktop_exec(exe).unwrap(),
            r#""/opt/aoproxy/aoproxy-gui" --autostart"#
        );
    }

    /// 空格靠引号，`$` 与 `"` 在文件里写成 `\\$`、`\\"`，反斜杠写成四个，`%` 写成两个。
    #[test]
    fn desktop_exec_escapes_reserved_characters() {
        let exe = Path::new(r#"/home/me/my apps/$x"y\z%/aoproxy-gui"#);
        assert_eq!(
            desktop_exec(exe).unwrap(),
            r#""/home/me/my apps/\\$x\\"y\\\\z%%/aoproxy-gui" --autostart"#
        );
    }

    #[test]
    fn desktop_exec_rejects_control_characters() {
        assert!(desktop_exec(Path::new("/tmp/a\nb")).is_err());
    }

    #[test]
    fn desktop_entry_round_trips_as_enabled() {
        let exe = Path::new("/opt/aoproxy/aoproxy-gui");
        let entry = desktop_entry(exe).unwrap();
        assert!(desktop_entry_enabled(&entry, exe));
        // 程序挪了位置：旧登记对新位置不算数。
        assert!(!desktop_entry_enabled(
            &entry,
            Path::new("/usr/bin/aoproxy-gui")
        ));
    }

    /// 在「启动应用程序」里被关掉的项，文件还在，但不能算开启。
    #[test]
    fn desktop_entry_disabled_by_desktop_environment() {
        let exe = Path::new("/opt/aoproxy/aoproxy-gui");
        let entry = desktop_entry(exe).unwrap();
        let gnome_off = entry.replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        assert!(!desktop_entry_enabled(&gnome_off, exe));
        assert!(!desktop_entry_enabled(
            &format!("{entry}Hidden=true\n"),
            exe
        ));
        // 别的分组里的同名键不作数。
        assert!(desktop_entry_enabled(
            &format!("{entry}[Desktop Action x]\nHidden=true\n"),
            exe
        ));
    }

    #[test]
    fn launch_agent_escapes_xml() {
        let plist = launch_agent_plist(Path::new("/Apps/A&B <x>/aoproxy-gui")).unwrap();
        assert!(
            plist.contains("<string>/Apps/A&amp;B &lt;x&gt;/aoproxy-gui</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>--autostart</string>"), "{plist}");
    }
}

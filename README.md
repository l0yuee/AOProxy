# AOProxy

面向 Claude Code、Codex 等 AI Agent 的代理转发工具。只负责搬运流量，不解析、不存储、不改写 API 密钥与模型参数。

提供命令行程序 `aoproxy` 与图形界面程序 `aoproxy-gui`，两者共用同一套转发引擎和同一份配置文件。

## 简介

```
场景 1 · 服务器端（CLI 部署）
  本机 Agent ──公网──▶ 服务器 AOProxy (0.0.0.0:端口) ──直连──▶ AI API

场景 2 · 本机端（GUI 或 CLI）
  本机 Agent ──▶ 本机 AOProxy (127.0.0.1:端口) ──代理──▶ 代理服务器 ──▶ AI API

两者串联
  本机 Agent ──▶ 本机 AOProxy ──TLS + 认证──▶ 服务器 AOProxy ──▶ AI API
```

一份配置可以有多条规则，每条规则监听一个端口，工作在以下两种模式之一：

| 模式 | 客户端如何接入 | 适用 |
|---|---|---|
| 反向 `reverse` | 把 Base URL 指向 AOProxy，请求一律转发到规则里的固定目标 | 使用 API Key 的 Agent |
| 正向 `forward` | 把 AOProxy 当作代理服务器；同一端口自动识别 SOCKS5、HTTP CONNECT、HTTP 代理 | 使用订阅账号（Claude Pro/Max、ChatGPT）登录的 Agent：OAuth 登录与令牌刷新会访问 API 以外的域名 |

反向模式只改写 `Host`、剥离逐跳首部，`Authorization`、`x-api-key` 等首部原样透传，流式（SSE）响应逐块转发、不缓冲。
正向模式的 SOCKS5 只支持 CONNECT。

每条规则的入站可加 TLS 与认证；出站可直连，也可经 HTTP、HTTPS、SOCKS5 上游代理。

---

## 安装

从源码构建，需要最新稳定版 [Rust](https://rustup.rs)（最低 1.88）。

### Windows

只需安装 Rust。rustup 安装时会检查 MSVC 生成工具，缺少时按提示装上即可——这是 Rust 在 Windows 上本身的要求。

GUI 用到的 Qt 6.8.3 已随项目放在 `third_party/qt`，由 `.cargo/config.toml` 按相对路径引用：不必另装 Qt，
不必设置环境变量，也不必进入 VS 开发者命令行。项目目录整个拷到别的机器上即可构建。

```powershell
cargo build --release
```

得到 `target\release\aoproxy.exe`（CLI）与 `target\release\aoproxy-gui.exe`（GUI），双击即可运行，
不需要再做别的事。

GUI 所需的 Qt 运行库由构建脚本调用随附的 windeployqt 自动收集到 exe 旁边（DLL、平台插件与 QML 模块，
共约 60 MB）。整个 `target\release` 目录可以拷到没装 Qt 的机器上直接用。想自行处理部署（例如换一套裁剪过
的 Qt），设 `AOPROXY_NO_QT_DEPLOY=1` 跳过这一步。

两个程序都依赖 Microsoft Visual C++ 运行库。构建机上已随 MSVC 装好；拷到别的机器上若提示缺少
`VCRUNTIME140.dll` 之类的文件，安装微软的
[VC++ 可再发行组件](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)即可。

### Linux / macOS

```bash
cargo build --release -p aoproxy
```

只构建 CLI，它不依赖 Qt，产物为 `target/release/aoproxy`。

**`-p aoproxy` 不能省。** 省掉就会连 GUI 一起构建，而 `.cargo/config.toml` 里的 `QMAKE` 指向随项目的
Windows 版 qmake（`third_party/qt` 只有 Windows 的 Qt），在 Linux 上执行它会失败，报错形如：

```
Could not find Qt installation: QMakeSetQtMissing {
    qmake_env_var: ".../third_party/qt/bin/qmake.exe",
    error: QmakeFailed(Os { code: 13, kind: PermissionDenied })
}
```

见到这个报错，要么加上 `-p aoproxy` 只构建 CLI，要么按下面的步骤配好系统 Qt 再构建 GUI。

#### 在 Linux / macOS 上构建 GUI

先装系统的 Qt 6（6.2 及以上）与 QML 模块。Debian / Ubuntu / Kali：

```bash
sudo apt install qt6-base-dev qt6-declarative-dev lld \
                 qml6-module-qtquick-controls qml6-module-qtquick-layouts \
                 qml6-module-qtqml-workerscript qml6-module-qt-labs-platform \
                 qml6-module-qtquick-dialogs
```

`lld` 是必需的：cxx-qt 的构建脚本在 Linux 上会发 `-fuse-ld=lld`，没装会在链接阶段失败。
`qml6-module-qt-labs-platform` 只影响托盘——缺了它界面照常启动，只是没有托盘图标。
`qml6-module-qtquick-dialogs` 只影响设置页选择配置文件的「浏览…」按钮，缺了它照样能手填路径。

再用 `QMAKE` 指向系统的 qmake（外部设置的值优先于 `.cargo/config.toml`，无需改仓库里的文件）：

```bash
QMAKE=$(command -v qmake6) cargo build --release -p aoproxy-gui
```

Debian 系的 qmake 在 `/usr/bin/qmake6`，不是 `/usr/lib/qt6/bin/qmake`；Arch 用 `qmake6`，
macOS 用 Homebrew 装的则在 `$(brew --prefix qt)/bin/qmake`。拿不准就用上面的 `command -v` 写法。

Windows 上那套自动收集 Qt 运行库的机制在这两个平台上不启用：GUI 直接链系统的 Qt，由包管理器负责运行库。

在 Linux 服务器上以 systemd 服务运行的完整步骤见 [docs/deploy-linux.md](docs/deploy-linux.md)。

---

## 使用

### 快速开始

以下假定 `aoproxy` 已在 `PATH` 中，否则换成 `target/release/aoproxy` 的完整路径。

```bash
aoproxy config init   # 在默认位置生成示例配置
aoproxy run           # 启动全部已启用的规则，Ctrl+C 退出
```

示例配置里有一条已启用的规则 `claude`：监听 `127.0.0.1:8080`，反向转发到 `https://api.anthropic.com`。
让 Claude Code 经它访问 API：

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
```

PowerShell 中写作 `$env:ANTHROPIC_BASE_URL = 'http://127.0.0.1:8080'`。

配置文件的位置以 `aoproxy config path` 的输出为准，默认是：

| 平台 | 默认路径 |
|---|---|
| Windows | `%APPDATA%\AOProxy\config\config.toml` |
| Linux | `~/.config/aoproxy/config.toml`（设置了 `XDG_CONFIG_HOME` 时位于其下） |
| macOS | `~/Library/Application Support/AOProxy/config.toml` |

配置文件也可以放在别处：在 GUI 的设置页换一个文件，这个位置会记在默认路径旁边的 `location.toml` 里，
此后 GUI 与 CLI 都用它；删掉 `location.toml` 就回到默认位置。命令行的 `-c` 优先于这一切，只作用于那一次运行。

### 命令行

`aoproxy` 不带子命令时等同于 `aoproxy run`。

```
aoproxy run [选项]

  -c, --config <FILE>       配置文件路径（省略时用 GUI 设置页选定的位置，没选过则为平台默认位置）
  -r, --rule <ID>           只启动指定规则（可重复；省略时启动全部已启用规则）
      --log-level <LEVEL>   日志级别 error | warn | info | debug | trace（默认 info）
  -q, --quiet               静默模式，不输出任何日志

临时规则（给出 --listen 时忽略配置文件中的规则，只运行命令行拼出的这一条，不写盘）：
      --listen <ADDR>       监听地址 主机:端口
      --mode <MODE>         forward | reverse（默认 forward）
      --target <URL>        转发目标，reverse 模式必填
      --auth <USER:PASS>    入站 Basic 认证
      --auth-token <TOKEN>  入站路径令牌（仅 reverse 模式，与 --auth 二选一）
      --tls-cert <FILE>     入站 TLS 证书链（PEM，与 --tls-key 同时给出）
      --tls-key <FILE>      入站 TLS 私钥（PEM）
      --upstream <URL>      出站上游 scheme://[用户名:密码@]主机:端口，
                            scheme 为 direct | http | https | socks5
```

```bash
# 只启动 claude 规则，输出 debug 日志
aoproxy run -r claude --log-level debug

# 指定配置文件，静默运行
aoproxy run -c /etc/aoproxy/config.toml -q

# 临时规则：本机 8080 反向代理到 Claude API，经 SOCKS5 出站
aoproxy run --listen 127.0.0.1:8080 --mode reverse --target https://api.anthropic.com \
            --upstream socks5://user:pass@127.0.0.1:1080
```

管理配置文件：

| 命令 | 作用 |
|---|---|
| `aoproxy config path` | 打印配置文件路径 |
| `aoproxy config init` | 生成示例配置（文件已存在则报错） |
| `aoproxy config check` | 校验配置文件 |
| `aoproxy config show` | 以 TOML 格式打印当前配置 |

以上命令都可用 `-c <FILE>` 指定其他配置文件。

`aoproxy run` 在配置文件不存在、或一条规则都没能启动（没有启用的规则、端口全被占用……）时报错退出，
退出码为 1，而不是守着一个什么都没在监听的进程——在 systemd 下这样 `Restart=on-failure` 才能起作用。
部分规则启动失败时，其余的照常运行。

### 图形界面

运行部署好的 `aoproxy-gui.exe`。它与 CLI 读写同一份配置文件，改动即时保存，没有「保存」按钮。

- **规则**：每条规则一张卡片，显示运行状态、监听地址、模式、流量与连接数；鼠标悬停时可编辑或删除。
  顶栏有「新建」「全部启用」「全部停止」。规则的「启用」开关打开即启动该规则、关上即停止，
  并决定它会不会被「全部启用」与开机自启一并启动；卡片上的「启动」「停止」只管这一次，不改开关。
- **日志**：运行日志。默认关闭，在设置页打开。
- **设置**：
  - 界面语言：默认「跟随系统」（中文系统用中文，其他一律英文），在这里选了具体语言之后以你的选择为准，
    选回「跟随系统」即恢复；切换即时生效。日志开关与级别、关闭窗口时是否最小化到托盘。
  - **登录系统后自动启动**：开机后程序缩在托盘里启动，并运行全部已启用的规则。登记在系统自己的自启位置：
    Windows 是注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，Linux 是
    `~/.config/autostart/aoproxy.desktop`，macOS 是 `~/Library/LaunchAgents/com.aoproxy.gui.plist`。
    开关显示的是系统里实际登记的状态：在任务管理器或桌面环境里关掉自启，这里也会跟着变。
    程序挪了位置后要重新打开一次。
  - **配置文件**：填另一个文件的完整路径后点「应用」（或点「浏览…」选择）。文件已存在就载入它；
    不存在就把当前配置存过去。定义没变的规则不会被打断，其余正在运行的规则会先停下。
    「恢复默认」回到平台默认位置。启动时配置文件读不出来（比如格式写错了），顶部会显示原因，
    在外面改好后回到这里点「应用」即可重新载入，不必重启。

托盘图标在有规则运行时为彩色、全部停止时为灰色；单击或双击唤回窗口。右键菜单：

- 显示主窗口；
- 「规则」子菜单（标题带运行计数，如「规则 · 运行中 1 / 3」）：每条规则一项，带运行状态
  （如「Claude Code · 运行中」）。勾选即规则卡片上的「启用」开关：勾上即启动，去掉即停止；
  显示的是配置里存下的开关，重启后照旧，配置里没写的规则算未启用；
- 全部启用、全部停止；
- 「日志」子菜单：开关日志、选择日志级别，与设置页里的是同一项设置；
- 退出。

系统托盘不可用时，关闭窗口即退出。退出时若仍有规则在运行会先确认，确认后停止全部规则再退出。

程序只运行一个实例：再次启动会把已打开的窗口唤到前台。

`aoproxy-gui` 也接受两个命令行参数：`-c <FILE>`（本次运行用这个配置文件，不改设置页记下的位置）与
`--autostart`（开机自启时由系统带上：缩在托盘里启动，并运行已启用的规则）。

### 配置示例

完整字段说明见 [docs/config-reference.md](docs/config-reference.md)。

**本机：两条反向规则，经 SOCKS5 上游出网**

```toml
version = 1

[app]                        # 整块只有 GUI 读取；CLI 的日志只看命令行选项
language         = "zh-CN"   # zh-CN | en-US
logging_enabled  = true
log_level        = "info"
minimize_to_tray = true

[[rules]]
id      = "claude"
name    = "Claude Code"
enabled = true
mode    = "reverse"
listen  = "127.0.0.1:8080"
target  = "https://api.anthropic.com"

[rules.upstream]
kind     = "socks5"
address  = "proxy.example.com:1080"
username = "user"
password = "pass"

[[rules]]
id      = "codex"
name    = "Codex"
enabled = true
mode    = "reverse"
listen  = "127.0.0.1:8081"
target  = "https://api.openai.com"

[rules.upstream]
kind    = "socks5"
address = "proxy.example.com:1080"
```

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
export OPENAI_BASE_URL=http://127.0.0.1:8081
```

**服务器：正向代理，TLS + Basic 认证**

```toml
version = 1

[[rules]]
id      = "gateway"
name    = "公网网关"
enabled = true
mode    = "forward"
listen  = "0.0.0.0:8443"

[rules.tls]
cert = "/etc/letsencrypt/live/example.com/fullchain.pem"
key  = "/etc/letsencrypt/live/example.com/privkey.pem"

[rules.auth]
kind     = "basic"
username = "alice"
password = "s3cret"
```

客户端可以直接把它当 HTTPS 代理：

```bash
export HTTPS_PROXY=https://alice:s3cret@example.com:8443
```

也可以让本机 AOProxy 的规则经它出站（即「两者串联」），在本机规则下写：

```toml
[rules.upstream]
kind     = "https"
address  = "example.com:8443"
username = "alice"
password = "s3cret"
```

### 认证

在规则下加 `[rules.auth]`：

| `kind` | 适用模式 | 客户端如何提供凭据 |
|---|---|---|
| `none` | 正向、反向 | 不认证（默认） |
| `basic` | 正向、反向 | 发送 `Proxy-Authorization: Basic …` 首部。正向模式下把凭据写进代理地址即可（如上面的 `HTTPS_PROXY`）；SOCKS5 客户端使用同一组用户名密码 |
| `token` | 仅反向 | 令牌作为 Base URL 路径的第一段，不匹配时返回 404 |

```toml
[rules.auth]
kind  = "token"
token = "my-secret-token"
```

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080/my-secret-token
```

反向模式的 Basic 认证同样读 `Proxy-Authorization`，不占用装着 API Key 的 `Authorization`。把凭据写进
Base URL 不起作用，客户端需要显式附加这个首部，例如 Claude Code：

```bash
export ANTHROPIC_CUSTOM_HEADERS='Proxy-Authorization: Basic YWxpY2U6czNjcmV0'   # base64("alice:s3cret")
```

客户端不便附加首部时，反向模式改用路径令牌。

### 出站上游

在规则下加 `[rules.upstream]`，省略即直连：

| `kind` | 说明 |
|---|---|
| `direct` | 直连目标（默认） |
| `http` | 经 HTTP 代理（CONNECT）出站 |
| `https` | 同上，到代理这一段走 TLS |
| `socks5` | 经 SOCKS5 代理出站 |

`address` 填 `主机:端口`；`username`、`password` 可选，须同时填写或同时留空。

### 入站 TLS

在规则下加 `[rules.tls]`，`cert`、`key` 分别指向 PEM 格式的证书链与私钥，二者必须配对（见上面的服务器示例）。

### 安全建议

- 未配置认证的规则只监听 `127.0.0.1`，或在防火墙上限制来源。
- 监听公网地址时同时启用 TLS 与认证：缺认证等于把转发能力开放给任何人，缺 TLS 则凭据以明文过网。
- 密码、API Key 与 `Authorization` 首部在任何日志级别下都不会输出。

---

## 测试

核心引擎、配置与命令行回归测试：

```bash
cargo test -p aoproxy-core -p aoproxy
```

Windows 下可用项目自带的 Qt 6.8.3 运行托盘菜单回归测试（需要 MSVC 生成工具）：

```powershell
powershell -ExecutionPolicy Bypass -File tools/test-qml.ps1
```

配置损坏后的 GUI 设置恢复另有 Rust 回归测试。在 Windows 下运行时将 Qt DLL 目录加入当前进程的 PATH：

```powershell
$env:PATH = (Resolve-Path third_party/qt/bin).Path + ';' + $env:PATH
cargo test -p aoproxy-gui --test recovery_settings
```

托盘测试在离屏模式运行真实的 QML 菜单，覆盖启用开关、失败回滚、外部状态同步、配置切换与语言切换。
其他平台可在单独的构建目录用系统 Qt 的 `qmake` 构建 `crates/gui/tests/qml_tests.pro`，
再以 `-platform offscreen -input <项目路径>/crates/gui/tests` 运行生成的测试程序。

---

## 许可证

MIT。`third_party/qt` 下随附的 Qt 按其自身许可证（LGPL-3.0 等）分发，详见
[third_party/qt/README.md](third_party/qt/README.md)。

# 配置文件参考

AOProxy 使用 TOML 格式的配置文件。本文档覆盖所有字段的类型、取值范围和默认值。

配置文件路径：

| 平台 | 默认路径 |
|---|---|
| Windows | `%APPDATA%\AOProxy\config\config.toml` |
| Linux | `~/.config/aoproxy/config.toml` |
| macOS | `~/Library/Application Support/AOProxy/config.toml` |

用 `aoproxy config path` 查看实际使用的路径，用 `aoproxy config init` 生成示例文件。

配置文件可以不在默认位置。GUI 设置页里换了配置文件后，新位置记在默认路径同目录的 `location.toml` 中：

```toml
config = "D:/proxy/aoproxy.toml"
```

GUI 与 CLI 按以下顺序确定用哪个文件：

1. 命令行 `-c <FILE>`（只作用于这一次运行，不改 `location.toml`）；
2. `location.toml` 里记的位置（相对路径按 `location.toml` 所在目录解析）；
3. 上表的默认路径。

删掉 `location.toml` 即回到默认位置。它写坏了会报错，而不是悄悄退回默认位置——那样改的就不是你以为的那份配置了。

---

## 顶层字段

```toml
version = 1
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `version` | 整数 | 是 | 配置格式版本，目前只有 `1` |

---

## `[app]` — 应用全局设置

```toml
[app]
language         = "zh-CN"
logging_enabled  = true
log_level        = "info"
minimize_to_tray = true
```

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `language` | 字符串 | 跟随系统 | 界面语言。`"zh-CN"`（简体中文）或 `"en-US"`（英文）；不写即跟随系统语言 |
| `logging_enabled` | 布尔 | `false` | 是否启用日志输出 |
| `log_level` | 字符串 | `"info"` | 日志级别，见下表 |
| `minimize_to_tray` | 布尔 | `true` | **GUI 专用**：关闭窗口时隐藏到托盘而不是退出。托盘不可用时该项无效，关窗按退出处理 |

`[app]` 整块只有 GUI 读取：启动时按 `logging_enabled`、`log_level` 初始化日志，设置页改动这些项
即时生效并落盘。CLI 不读这一块，它的日志只由命令行决定——默认 `info` 级别、默认开启，
用 `--log-level` 改级别、`--quiet` 关掉。`minimize_to_tray` 也只有 GUI 用得上。

界面语言的规则：

- 没写 `language` 时跟随系统语言。系统是中文（简繁都算）用简体中文，其余一律英文——只有这两份译文，
  法语、日语系统上与其给一份看不懂的中文，不如给英文。系统语言这样取：Windows 读用户界面语言；
  Linux 等按 gettext 的规矩读环境变量（`LC_ALL` > `LC_MESSAGES` > `LANG` 定出区域，区域不是
  `C`/`POSIX` 时 `LANGUAGE` 更优先）；macOS 从访达启动时没有这些变量，读系统偏好里的首选语言。
  什么都取不到时用英文。
- 在设置页选过语言之后，以选的为准，这时才会写进 `language`。只改别的设置（日志、托盘……）不会顺手把
  当时的系统语言记成你的选择，系统语言变了照样跟着变。想回到跟随系统，删掉这一行即可。
- 设置页的语言下拉框显示的总是界面实际在用的语言。

CLI 不读 `language`，始终跟随系统语言。

`log_level` 取值：

| 值 | 说明 |
|---|---|
| `"error"` | 只输出错误 |
| `"warn"` | 错误和警告 |
| `"info"` | 常规运行信息（默认） |
| `"debug"` | 调试详情（请求/响应首部等） |
| `"trace"` | 最详细，含协议协商过程 |

此字段供 GUI 使用。CLI 的日志级别由 `--log-level` 指定，默认 `info`，与此字段无关。

---

## `[[rules]]` — 代理规则

每条规则对应一个监听端口。可以定义多条规则，每条规则独立运行。

```toml
[[rules]]
id      = "claude"
name    = "Claude Code"
enabled = true
mode    = "reverse"
listen  = "127.0.0.1:8080"
target  = "https://api.anthropic.com"
```

### 基础字段

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | 字符串 | 是 | 规则唯一标识符。只允许字母、数字、连字符（`-`）和下划线（`_`），不能为空 |
| `name` | 字符串 | 否 | 人类可读的显示名称，GUI 和日志中使用 |
| `enabled` | 布尔 | 否 | 是否启用：`aoproxy run`、GUI 的「全部启用」与开机自启只启动启用的规则。不写即未启用 |
| `mode` | 字符串 | 是 | `"forward"`（正向代理）或 `"reverse"`（反向代理）|
| `listen` | 字符串 | 是 | 监听地址，格式 `host:port`。本地部署用 `127.0.0.1`，公网用 `0.0.0.0` |
| `target` | 字符串 | 反向模式必填 | 转发目标，格式 `scheme://host`，scheme 必须是 `http` 或 `https` |

### `[rules.upstream]` — 出站上游

省略此段时默认直连（等同于 `kind = "direct"`）。

```toml
[rules.upstream]
kind     = "socks5"
address  = "proxy.example.com:1080"
username = "user"
password = "pass"
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `kind` | 字符串 | 是 | 上游类型，见下表 |
| `address` | 字符串 | 非 `direct` 时必填 | 代理服务器地址，格式 `host:port` |
| `username` | 字符串 | 否 | 代理用户名（`direct` 无效）|
| `password` | 字符串 | 否 | 代理密码，必须与 `username` 同时填写或同时留空 |

`kind` 取值：

| 值 | 说明 |
|---|---|
| `"direct"` | 直接连接目标，不经过代理（默认）|
| `"http"` | 通过明文 HTTP CONNECT 代理出站 |
| `"https"` | 通过 TLS HTTP CONNECT 代理出站 |
| `"socks5"` | 通过 SOCKS5 代理出站，支持 RFC 1929 用户名密码认证 |

### `[rules.auth]` — 入站认证

省略此段时不验证客户端身份（等同于 `kind = "none"`）。

```toml
[rules.auth]
kind     = "basic"
username = "alice"
password = "s3cret"
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `kind` | 字符串 | 是 | 认证类型，见下表 |
| `username` | 字符串 | `basic` 必填 | Basic 认证用户名 |
| `password` | 字符串 | `basic` 必填 | Basic 认证密码 |
| `token` | 字符串 | `token` 必填 | 路径令牌（仅反向模式），不能为空 |

`kind` 取值：

| 值 | 适用模式 | 说明 |
|---|---|---|
| `"none"` | 正向 / 反向 | 不验证，本地部署时使用 |
| `"basic"` | 正向 / 反向 | HTTP Basic 认证。两种模式都读取 `Proxy-Authorization` 首部，验证通过后按逐跳首部剥掉，不会转发给上游；客户端的 `Authorization`、`x-api-key` 不受影响。SOCKS5 入站使用 RFC 1929 子协商，密码与此处 `username`/`password` 共用 |
| `"token"` | **仅反向** | 令牌作为 URL 路径的第一段。不匹配时返回 404，令牌不写入任何日志 |

**注意**：`"token"` 只能用于反向模式（`mode = "reverse"`），在正向模式下配置会报校验错误。

#### Basic 认证示例

正向模式下把凭据写进代理地址，由客户端自行生成 `Proxy-Authorization`：

```bash
export HTTPS_PROXY=https://alice:s3cret@example.com:8443
```

反向模式读的同样是 `Proxy-Authorization`，而客户端从 Base URL 里的凭据生成的是
`Authorization`——那一头装着 API Key，不能占用。所以要由客户端显式附加首部，例如 Claude Code：

```bash
export ANTHROPIC_CUSTOM_HEADERS='Proxy-Authorization: Basic YWxpY2U6czNjcmV0'   # base64("alice:s3cret")
```

客户端不便附加首部时，反向模式改用下面的路径令牌。

验证失败时返回 `407 Proxy Authentication Required`，并带 `Proxy-Authenticate: Basic realm="AOProxy"`。

#### Token 认证示例

配置：
```toml
[rules.auth]
kind  = "token"
token = "abc123"
```

客户端：
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080/abc123
```

AOProxy 会剥除路径中的令牌前缀后再转发，目标服务器看到的路径不含令牌。

### `[rules.tls]` — 入站 TLS

省略此段时入站连接为明文。

```toml
[rules.tls]
cert = "/etc/ssl/certs/fullchain.pem"
key  = "/etc/ssl/private/privkey.pem"
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `cert` | 字符串 | 是 | PEM 格式证书链文件路径（含中间证书）|
| `key` | 字符串 | 是 | PEM 格式私钥文件路径：PKCS#8（`BEGIN PRIVATE KEY`）、RSA（`BEGIN RSA PRIVATE KEY`）或 EC（`BEGIN EC PRIVATE KEY`）均可 |

正向、反向两种模式都可以配入站 TLS：反向模式因此成为 HTTPS 服务端，正向模式则成为 HTTPS 代理。

配置文件加载时检查文件存在性；证书与私钥的匹配性在引擎启动时验证。

---

## 完整示例

### 多规则配置

```toml
version = 1

[app]
language         = "zh-CN"
logging_enabled  = true
log_level        = "info"
minimize_to_tray = true

# ── 规则 1：本机反向代理 Claude，经 SOCKS5 出站 ──
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

# ── 规则 2：本机反向代理 OpenAI，经同一个 SOCKS5 出站 ──
[[rules]]
id      = "openai"
name    = "OpenAI"
enabled = true
mode    = "reverse"
listen  = "127.0.0.1:8081"
target  = "https://api.openai.com"

[rules.upstream]
kind    = "socks5"
address = "proxy.example.com:1080"

# ── 规则 3：公网正向代理，TLS 入站 + Basic 认证 ──
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

# ── 规则 4：本机反向代理，路径令牌认证 ──
[[rules]]
id      = "claude-token"
name    = "Claude（令牌保护）"
enabled = false
mode    = "reverse"
listen  = "127.0.0.1:8082"
target  = "https://api.anthropic.com"

[rules.auth]
kind  = "token"
token = "my-secret-token"
```

---

## 校验规则汇总

配置加载（启动或 `aoproxy config check`）时，以下条件任一不满足则报错：

- `version` 必须为 `1`
- 全文不能有未知字段：所有段都是 `deny_unknown_fields`，键名拼错会整份拒掉而不是静默忽略
- `rules[*].id` 不能重复
- 不同规则的 `listen` 解析后不能指向同一个地址端口
- `rules[*].id` 不能为空，只含字母、数字、`-`、`_`
- `rules[*].listen` 必须是合法的 `host:port`
- 反向模式（`mode = "reverse"`）必须提供 `target`
- `target` 必须带 `http://` 或 `https://` scheme，且包含主机名
- 非直连上游（`upstream.kind != "direct"`）必须提供 `address`
- `upstream.username` 与 `upstream.password` 必须同时填写或同时留空
- Basic 认证必须同时提供 `username` 和 `password`
- Token 认证只能用于反向模式，且 `token` 不能为空
- TLS 配置的 `cert` 和 `key` 文件必须存在（路径在加载时校验）

`aoproxy config check` 在上述校验之外多做一步：读取证书与私钥，确认两者配对。
这一步要解析密钥文件，所以只在 `config check`、规则启动和 GUI 保存规则时做，
不在每次加载配置时做。GUI 的规则编辑对话框走的是同一个函数，结论与 CLI 一致。

---

## 与 CLI 选项的关系

`aoproxy run` 的命令行选项都只作用于本次运行，不修改配置文件：

| CLI 选项 | 作用 |
|---|---|
| `--log-level <级别>` | 本次运行的日志级别，默认 `info`。CLI 不读 `app.log_level` |
| `--quiet` | 本次运行不输出日志，优先于 `--log-level` |
| `--rule <ID>` | 只启动指定规则，其余 `enabled = true` 的规则不启动 |

给出 `--listen` 时走临时规则：整个配置文件都不参与，只运行命令行拼出的这一条，也不写盘。

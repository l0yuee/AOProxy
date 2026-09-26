# Linux 服务器部署

服务器端只用 CLI（`aoproxy`），不依赖 Qt。典型用途是场景 1：公网正向代理，
供本机的 AOProxy 或 Agent 经 TLS + 认证接入。

本文以 Debian 12 / Ubuntu 22.04 与 systemd 为例，其他发行版替换包管理器与路径即可。

---

## 1. 构建

在服务器上直接构建（需要 Rust 1.88+）：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

cd aoproxy
cargo build --release -p aoproxy
```

`-p aoproxy` 只构建 CLI。不要用不带 `-p` 的 `cargo build --release`——那会连 GUI 一起构建，
在没有 Qt 的服务器上必然失败。

也可以在本机构建后只把二进制传上去：

```bash
cargo build --release -p aoproxy --target x86_64-unknown-linux-gnu
scp target/x86_64-unknown-linux-gnu/release/aoproxy user@server:/tmp/
```

产物是 `target/release/aoproxy`，除 glibc 外没有运行时依赖。

---

## 2. 安装

```bash
sudo install -m 0755 target/release/aoproxy /usr/local/bin/aoproxy
aoproxy --version
```

建一个不能登录的系统用户专门跑这个服务：

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin aoproxy
```

配置文件放 `/etc/aoproxy/config.toml`。里面可能含认证密码，权限收到 0600、属主给服务用户：

```bash
sudo mkdir -p /etc/aoproxy
sudo aoproxy config init -c /etc/aoproxy/config.toml
sudo chown -R aoproxy:aoproxy /etc/aoproxy
sudo chmod 0700 /etc/aoproxy
sudo chmod 0600 /etc/aoproxy/config.toml
```

服务用户的默认配置路径（`~/.config/aoproxy/config.toml`）在 `--no-create-home` 下不存在，
所以每条命令都要显式带 `-c`。

`config init` 生成的是示例配置（两条反向规则），按下一节改成服务器端要的规则，改完校验：

```bash
sudo -u aoproxy aoproxy config check -c /etc/aoproxy/config.toml
```

`config check` 除字段校验外还会读证书确认私钥配对，所以要用服务用户的身份跑。
以 root 通过、以 `aoproxy` 失败，说明是证书文件的权限问题，见第 5 节。

---

## 3. 服务器端配置

公网入口：正向模式 + TLS + Basic 认证。正向模式一个端口自适应 SOCKS5、HTTP CONNECT
和 HTTP 代理三种协议，本机端不必与服务器约定用哪种。

```toml
version = 1

[app]
logging_enabled = true
log_level       = "info"

[[rules]]
id      = "gateway"
name    = "公网网关"
enabled = true
mode    = "forward"
listen  = "0.0.0.0:8443"

[rules.tls]
cert = "/etc/aoproxy/tls/fullchain.pem"
key  = "/etc/aoproxy/tls/privkey.pem"

[rules.auth]
kind     = "basic"
username = "alice"
password = "换成足够长的随机串"
```

公网监听必须同时配 TLS 和认证：缺认证等于把转发能力开放给任何扫到这个端口的人，
缺 TLS 则 `Proxy-Authorization` 会以明文过网。

本机端在 `[rules.upstream]` 里指向它：

```toml
[rules.upstream]
kind     = "https"
address  = "example.com:8443"
username = "alice"
password = "换成足够长的随机串"
```

字段含义与取值范围见 [config-reference.md](config-reference.md)。

---

## 4. systemd 单元

写入 `/etc/systemd/system/aoproxy.service`：

```ini
[Unit]
Description=AOProxy · AI Agent 代理转发
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=aoproxy
Group=aoproxy
ExecStart=/usr/local/bin/aoproxy run -c /etc/aoproxy/config.toml
Restart=on-failure
RestartSec=3
# SIGTERM 会走优雅停机：先停掉全部规则、等监听端口释放，再退出。
TimeoutStopSec=15

# ── 加固 ──
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

[Install]
WantedBy=multi-user.target
```

`ProtectSystem=strict` 把整个文件系统变成只读，但读取不受影响——CLI 不写配置文件
（改配置是 GUI 的事），所以不需要 `ReadWritePaths`。`AF_UNIX` 要留着：
名字解析可能经过本地套接字。

一条规则都没能启动时（端口被占、证书读不了、配置里没有启用的规则），`aoproxy run` 报错并以
退出码 1 结束，而不是空转——`Restart=on-failure` 于是会按 `RestartSec` 重试，
`systemctl status` 也不会把一个什么都没在监听的进程显示成 `active (running)`。

启用并启动：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now aoproxy
systemctl status aoproxy
```

`status` 里应当能看到监听行：

```
INFO  AOProxy 0.1.0
INFO  配置文件: /etc/aoproxy/config.toml
INFO  [gateway] 监听 0.0.0.0:8443 · 正向
```

改了配置文件后重启生效（不支持在线重载）：

```bash
sudo systemctl restart aoproxy
```

想核对加固效果：`systemd-analyze security aoproxy.service`。

---

## 5. 证书与权限

用 certbot 签发（`--standalone` 需要 80 端口临时空闲）：

```bash
sudo apt install certbot
sudo certbot certonly --standalone -d example.com
```

`/etc/letsencrypt/live/` 下的私钥默认只有 root 可读，而服务以 `aoproxy` 身份运行，
直接填那里的路径会在启动时报读取失败。把证书复制到服务自己的目录，并在续期后重复这一步：

```bash
sudo mkdir -p /etc/aoproxy/tls
sudo install -o aoproxy -g aoproxy -m 0644 \
  /etc/letsencrypt/live/example.com/fullchain.pem /etc/aoproxy/tls/fullchain.pem
sudo install -o aoproxy -g aoproxy -m 0600 \
  /etc/letsencrypt/live/example.com/privkey.pem  /etc/aoproxy/tls/privkey.pem
```

让 certbot 续期后自动做这件事，写 `/etc/letsencrypt/renewal-hooks/deploy/aoproxy.sh`
（`chmod 0755`）：

```bash
#!/bin/sh
set -e
D=/etc/letsencrypt/live/example.com
install -o aoproxy -g aoproxy -m 0644 "$D/fullchain.pem" /etc/aoproxy/tls/fullchain.pem
install -o aoproxy -g aoproxy -m 0600 "$D/privkey.pem"   /etc/aoproxy/tls/privkey.pem
systemctl restart aoproxy
```

证书链与私钥是否配对在规则启动时验证，也可以提前单独查：

```bash
sudo -u aoproxy aoproxy config check -c /etc/aoproxy/config.toml
```

### 监听 1024 以下的端口

非 root 进程默认绑不上 443 这类特权端口。给单元加一行，而不是改用 root 运行：

```ini
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

---

## 6. 防火墙

只放开代理端口。`ufw`：

```bash
sudo ufw allow 8443/tcp comment 'AOProxy'
sudo ufw status
```

`firewalld`：

```bash
sudo firewall-cmd --permanent --add-port=8443/tcp
sudo firewall-cmd --reload
```

能限制来源就限制，公网全开只在客户端 IP 不固定时才有必要：

```bash
sudo ufw allow from 203.0.113.0/24 to any port 8443 proto tcp
```

---

## 7. 日志

日志走 stderr，由 journald 收集：

```bash
# 跟踪
journalctl -u aoproxy -f

# 今天的错误
journalctl -u aoproxy --since today -p err

# 最近 200 行
journalctl -u aoproxy -n 200
```

临时提高级别排查（改单元里的 `ExecStart`，加 `--log-level debug`，重启）：

```
ExecStart=/usr/local/bin/aoproxy run -c /etc/aoproxy/config.toml --log-level debug
```

密码、API Key 和 `Authorization` 首部在任何级别下都不输出，`debug` 也一样；
路径令牌同样不写日志。所以贴 journal 排查时不必手工脱敏。

日志量大时给 journald 限容，别让它吃满磁盘（`/etc/systemd/journald.conf`）：

```ini
[Journal]
SystemMaxUse=200M
```

---

## 8. 升级

```bash
cd aoproxy && git pull
cargo build --release -p aoproxy
sudo systemctl stop aoproxy
sudo install -m 0755 target/release/aoproxy /usr/local/bin/aoproxy
sudo -u aoproxy aoproxy config check -c /etc/aoproxy/config.toml
sudo systemctl start aoproxy
```

先 `stop` 再换文件：正在运行的进程持有已打开的可执行文件，直接覆盖在 Linux 上虽然允许，
但换来的是新旧混用的状态。`config check` 放在启动前，配置格式跨版本变化时能早一步发现。

---

## 9. 排查

| 现象 | 原因与处理 |
|---|---|
| `Address already in use` | 端口被占。`sudo ss -tlnp \| grep 8443` 看是谁，或改 `listen` |
| `Permission denied` 绑定失败 | 端口 < 1024 而进程非 root。加 `AmbientCapabilities=CAP_NET_BIND_SERVICE` |
| 启动即报证书读取失败 | 私钥属主/权限不对。见第 5 节，别直接指向 `/etc/letsencrypt/live` |
| 报「没有私钥」 | 私钥文件里没有 PEM 格式的私钥。PKCS#8、RSA 与 EC（`BEGIN EC PRIVATE KEY`）格式都支持，加密过的私钥不支持 |
| 启动即报证书与私钥不匹配 | `cert` 和 `key` 来自不同签发批次，重新复制同一批 |
| 客户端连上就断 | 认证失败。journal 里有 `<IP>:<端口> 认证失败`，核对本机端 upstream 的用户名密码 |
| 客户端超时、服务端无日志 | 流量没到进程。检查防火墙与云厂商安全组 |
| `systemctl status` 显示 `code=exited, status=1` | 配置校验没过，或一条规则都没能启动（端口被占、证书读不了、没有启用的规则）。`journalctl -u aoproxy -n 20` 看具体那一条 |
| 服务反复重启 | `Restart=on-failure` 在掩盖一个必然失败。先 `systemctl stop`，再用服务用户身份前台跑一次 |

前台跑一次是最快的定位手段，错误直接打在终端上：

```bash
sudo -u aoproxy /usr/local/bin/aoproxy run -c /etc/aoproxy/config.toml --log-level debug
```

---

## 10. 卸载

```bash
sudo systemctl disable --now aoproxy
sudo rm /etc/systemd/system/aoproxy.service
sudo systemctl daemon-reload
sudo rm /usr/local/bin/aoproxy
sudo rm -rf /etc/aoproxy        # 含配置与证书副本，确认后再删
sudo userdel aoproxy
```

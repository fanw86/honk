# ShadowQUIC 集成分支

版本化 fork 预发行使用 `shadowquic-vMAJOR.MINOR.PATCH-beta.N` 标签，由专用 **ShadowQUIC release** 工作流重建两个架构并发布固定 UI 的二进制、对应源码及 `SHA256SUMS`。标准 `v*` 标签仍使用独立的上游发行配置。

实验包包含可选的 JLS ShadowQUIC 出站及经过测试的 doona 完整版与精简版。上游标准发行包不包含此集成功能。手动 **Fork build** 工作流需设置 `shadowquic=true`、完整的 `doona_sha`，并选择 `x86_64-unknown-linux-musl` 或 `aarch64-unknown-linux-musl`；默认编译特性不变。

## 校验软件包和源码

从同一次 Actions 运行下载二进制 artifact 和匹配的 `fork-sources-<target>` artifact，将两个 ZIP 解压至同一目录：

```sh
sha256sum -c SHA256SUMS
mkdir -p unpacked
tar -xzf honk-core-*.tar.gz --strip-components=1 -C unpacked
cat unpacked/BUILD-INFO.txt
```

清单记录两份源码的版本、架构、特性及构建链接。重新分发时保留两份源码归档、许可证和两个 UI 的声明目录。Actions artifact 有保留期限，请及时下载；此工作流不创建 GitHub Release。

## 安装

使用 `uname -m` 选择匹配的架构。这些是 Linux 二进制文件，不能直接在 Windows 执行。先阅读 [README 安装要求](../../../README.zh.md#安装)，确认内核、接口、路由和 DNS 设置。

```sh
sudo install -m 755 unpacked/honk-core /usr/bin/honk-core
sudo install -m 644 unpacked/honk-core.service /etc/systemd/system/honk-core.service
sudo mkdir -p /etc/honk
```

替换前保存现有程序及配置。新安装可将 `unpacked/config.min.dae` 复制到 `/etc/honk/config.dae`，并按机器实际情况修改接口、DNS 与路由。其中 SOCKS 节点及接口名只是示例，并非可直接运行的网关配置。按需替换节点、分组及路由：

```dae
node {
    sq: 'shadowquic://USERNAME:PASSWORD@SERVER:443?sni=JLS_SERVER_NAME#sq'
}
group {
    shadow { filter: name('sq')
             policy: fixed(0) }
}
routing { fallback: shadow }
```

凭据中的保留字符需百分号编码，SNI 必须与真实 JLS 服务器一致。[节点选项](../reference/nodes.md)说明 UDP 流模式、ALPN、MTU 和半关闭超时。启用透明代理前，补齐适合本机的本地流量绕过和路由规则。

## 内嵌 UI 和可编辑配置

在主配置中加入 `include { 'auth.dae' }`，本地创建 `/etc/honk/auth.dae`，将令牌替换为足够长的随机值：

```dae
experimental {
    native_api {
        enabled: true
        listen: '127.0.0.1:9527'
        secret: 'REPLACE_WITH_A_LONG_RANDOM_TOKEN'
        config_write: true
        ui: embedded
    }
}
```

引擎禁止通过 API 改写凭据 include，主文件则可保留可编辑的节点和分组。使用 `ui: embedded-lite` 选择精简版；两版都已内嵌，所选 UI 位于 `/ui/`。修改原生 API 设置需本地编辑并重启。认证和写入限制见[原生 API 配置](../reference/experimental.md)。

检查完整配置后执行：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now honk-core
sudo journalctl -u honk-core -n 100 --no-pager
```

在工作站执行 `ssh -L 9527:127.0.0.1:9527 USER@HOST`，打开 `http://127.0.0.1:9527/ui/` 并输入令牌。ShadowQUIC 表单要求用户名、密码和 JLS SNI。支持原生明文/Base64 URI 订阅，不支持虚构的 Clash/sing-box/Surge ShadowQUIC 结构化格式。

## 验证范围

处理器已有参考服务器 TCP/UDP、认证和生命周期测试；两个 UI 已有单元、构建及定向 Chromium 测试。打包检查 eBPF BTF 和归档，x86_64 还执行包内程序的 `--version` 与 `--help`。ARM64 仅交叉编译，未执行。这些检查不代表真实网关的 eBPF、内核/NFQUEUE、路由或部署验证。仍未启用早期应用数据重放。依赖此实验包前，请在目标机器确认连通性、UDP、API 访问和正常退出。

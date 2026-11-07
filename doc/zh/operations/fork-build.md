# 构建 honk 和 doona 分支

在 `fanw86/honk` 中打开 **Actions → Fork build → Run workflow**，选择 honk 分支、Linux musl 目标架构（`x86_64` 或 `aarch64`），并填写要嵌入的 `fanw86/doona` 完整 40 位提交 SHA。默认 SHA 固定为经过测试的集成 UI。可选的 `shadowquic` 输入默认关闭，启用后才包含实验性处理器。

工作流检查标准版 doona 并构建两个 UI 版本，打包许可证声明，然后嵌入包含 eBPF、Clash API、原生 API、rprx 和 jemalloc 的完整 honk 二进制。工具链和交叉编译步骤来自 `release.yml`；同步上游时应保持一致。

请在 14 天内从运行页面的 **Artifacts** 下载二进制、`SHA256SUMS` 和对应源码归档。`BUILD-INFO.txt` 标明两个输入提交及特性。工作流不发布 release 或标签。x86_64 软件包会执行版本和帮助启动检查；ARM64 仅交叉编译。启用 ShadowQUIC 的产物说明和验证范围见[安装指南](shadowquic-fork.md)。

公开 UI 分支无需额外配置仓库密钥。本地使用 `origin` 推送到个人分支，使用 `upstream` 获取上游更新。下载后按照常规安装指南部署。

此工作流不运行 honk 的 Rust 测试套件。产物构建成功不等同于通过完整上游 CI。

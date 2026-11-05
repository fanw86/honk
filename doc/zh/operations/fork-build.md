# 构建 honk 和 doona 分支

在 `fanw86/honk` 中打开 **Actions → Fork build → Run workflow**，选择 honk 分支、Linux musl 目标架构（`x86_64` 或 `aarch64`），并填写要嵌入的 `fanw86/doona` 完整 40 位提交 SHA。默认 SHA 对应最初的源码版本；构建 UI 修改时请更新它。

工作流检查标准版 doona 并构建两个 UI 版本，打包许可证声明，然后嵌入包含 eBPF、Clash API、原生 API、rprx 和 jemalloc 的完整 honk 二进制。工具链和交叉编译步骤来自 `release.yml`；同步上游时应保持一致。

请在 14 天内从运行页面的 **Artifacts** 下载二进制和对应源码归档。版本记录文件标明两个输入提交。工作流不发布 release 或标签。这只是构建基础设施，不会添加 ShadowQUIC 支持，也不能替代内核及协议互通测试。x86_64 二进制会执行版本启动检查；ARM64 交叉编译产物需要在目标设备验证。

公开 UI 分支无需额外配置仓库密钥。本地使用 `origin` 推送到个人分支，使用 `upstream` 获取上游更新。下载后按照常规安装指南部署。

此工作流不运行 honk 的 Rust 测试套件。产物构建成功不等同于通过完整上游 CI。

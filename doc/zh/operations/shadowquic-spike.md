# 验证候选 ShadowQUIC 客户端

实现出站前，在 honk 分支运行 **Actions → ShadowQUIC JLS spike → Run workflow**。工作流使用独立的 [spike](../../../spike/README.md)，连接固定提交版本的参考服务器，不修改生产引擎。

成功运行必须完成正确凭据认证、使用 IPv4 和域名地址通过多个流回显 TCP 数据，并拒绝错误密码。查看任务摘要，在 14 天内下载 `shadowquic-phase0-evidence`，获取工具链版本、提交 ID、已解析的依赖锁文件和服务端及客户端日志。失败或超时不能作为 GO 结论，必须先分析日志。

将实际运行链接和结果记录到集成工作区的 `plan.md`。此门槛只验证候选 JLS/TCP 兼容性，不覆盖 UDP、0-RTT、内核钩子或生产 honk 运行时。本地环境要求和命令见 spike README。

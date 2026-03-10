# mini-daytona-rs 技术文档

本目录包含 mini-daytona-rs 项目的技术实现文档。

当前文档已覆盖最新的双隔离档位设计：

- `Build Profile`：用于 Dockerfile `RUN`
- `Runtime Profile`：用于后台运行沙箱与 `exec`
- `pivot_root`、user namespace、cgroups v2、seccomp、`PR_SET_NO_NEW_PRIVS`
- 网络命名空间、bridge/NAT、DNS 注入，以及其与 Build/Runtime profile 的协作关系

| 文档 | 内容 |
|------|------|
| [architecture.md](architecture.md) | 系统架构设计 |
| [modules.md](modules.md) | 模块详细实现说明 |
| [security.md](security.md) | 安全设计、隔离模型与已知边界 |
| [api.md](api.md) | HTTP API 接口文档 |
| [networking.md](networking.md) | 网络隔离实现 |
| [build-system.md](build-system.md) | Dockerfile 构建与分层缓存机制 |

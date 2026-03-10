# Mini Daytona Rust (mini-daytona-rs)

`mini-daytona-rs` 是一个演示级别、超轻量级的容器运行时。它使用 Rust 编写，直接调用 Linux 内核能力，通过 namespace、OverlayFS、seccomp、cgroups 和 bridge/veth 网络提供接近容器的沙盒（Sandbox）体验。

## 目标与场景

这个项目旨在展示一个微缩版的沙盒控制面，它可以：
1. **解析 Dockerfile**：支持 `FROM`, `RUN`, `ENV`, `WORKDIR`, `USER`, `EXPOSE`, `COPY` 等指令。
2. **构建 Snapshot**：从远程 Registry 拉取镜像层，利用 OverlayFS 进行指令级缓存构建，当前默认产出目录型 snapshot。
3. **运行独立沙盒（Sandbox）**：通过 `CLONE_NEWUSER`, `CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWUTS`, `CLONE_NEWIPC`, `CLONE_NEWNET` 等 namespace 技术提供隔离，并通过 `veth` 网桥与 `iptables NAT` 维持互联网连通性。
4. **服务级 API 控制**：提供了完整生命周期的 HTTP 接口管控。
5. **双隔离档位**：Build 阶段强调兼容包管理器与构建脚本，Runtime 阶段强调收紧 capability、seccomp、`/proc` 和 `/sys` 暴露面。

## 架构说明

项目的具体底层技术实现请参阅 `doc/` 目录：

- [doc/architecture.md](./doc/architecture.md)
- [doc/modules.md](./doc/modules.md)
- [doc/security.md](./doc/security.md)
- [doc/networking.md](./doc/networking.md)
- [doc/build-system.md](./doc/build-system.md)
- [doc/api.md](./doc/api.md)

## 构建与运行

当前推荐在 Docker 中启动，因为项目依赖 Linux namespace、OverlayFS、iptables 和 cgroups 等特性。

## 快速开始

启动项目的最合适方式是使用集成好的 Docker 环境，因为其依赖于纯净的 Linux 环境层特性（特权级权限、Namespace隔离等）：

```bash
# 1. 启动服务（会自动构建镜像并以 privileged 模式启动）
bash start-docker.sh

# 2. 运行端到端客户端测试（Node.js 通过 3000 端口测试 API）
node test/run_e2e.js --client
```

如果需要强制重建镜像：

```bash
REBUILD=1 NO_CACHE=1 bash start-docker.sh
```

验证过的集成测试覆盖：

- Nginx 静态文件场景
- Python 脚本执行
- Pandas / Excel 数据处理
- 文件上传 / 下载
- 资源限制
- Puppeteer + Chromium

## REST API 接口定义

关于系统对外暴露的 REST API，请查阅 [doc/api.md](./doc/api.md)。

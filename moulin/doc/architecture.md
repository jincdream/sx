# 系统架构设计

## 概述

moulin 是一个用 Rust 从零构建的极简容器运行时，直接调用 Linux 内核 API 实现容器隔离。项目核心能力包括：

- **Dockerfile 解析与构建**：支持多条 Dockerfile 指令，具备分层构建与指令缓存
- **OverlayFS 分层文件系统**：实现写时复制（CoW）隔离机制
- **Linux Namespace 沙箱**：通过 User / PID / Mount / UTS / IPC / Network Namespace 实现进程隔离
- **分级安全策略**：构建阶段使用较宽松的 Build Profile，运行阶段使用更严格的 Runtime Profile
- **Seccomp + cgroups v2**：默认 seccomp 白名单与资源限制，运行态进一步收紧系统调用面
- **Docker Registry v2 客户端**：从 Docker Hub 等 Registry 拉取基础镜像
- **HTTP API Server**：基于 Axum 的 REST API，支持远程管理

## 整体架构

```text
┌──────────────────────────────────────────────────────────┐
│                      CLI / HTTP API                      │
│                  (clap / axum + tokio)                    │
├──────────┬──────────┬──────────┬──────────┬───────────────┤
│  build   │  start   │ snapshot │  list    │   destroy     │
│  引擎    │  沙箱    │  打包    │  查询    │   清理        │
├──────────┴──────────┴──────────┴──────────┴───────────────┤
│                    核心模块层                              │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ overlay  │ │ sandbox  │ │ snapshot │ │   metadata   │ │
│  │ OverlayFS│ │ 进程隔离 │ │ 快照目录 │ │   JSON 存储  │ │
│  └──────────┘ └──────────┘ └──────────┘ └──────────────┘ │
│  ┌──────────┐ ┌───────────────────────────────────────┐  │
│  │  netns   │ │           build 子系统                 │  │
│  │ 网络隔离 │ │  parser │ registry │ 构建引擎          │  │
│  └──────────┘ └───────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────┤
│                 Linux Kernel APIs                        │
│ clone(2) · mount(2) · pivot_root(2) · unshare(2)       │
│ seccomp · cgroups v2 · OverlayFS · veth · iptables     │
└──────────────────────────────────────────────────────────┘
```

## 数据流

### Dockerfile 构建流程

```text
Dockerfile ──parse──▶ Vec<Instruction>
                            │
                  ┌─────────┼─────────┐
                  ▼         ▼         ▼
               FROM       RUN     COPY/ADD
                │          │         │
                ▼          ▼         ▼
          Registry     OverlayFS  OverlayFS
          pull_image   mount      mount
                      │     + sandbox   + fs::copy
                │     + exec      │
                ▼          ▼         ▼
           Layer 0     Layer N    Layer N+1
                │          │         │
                └──────────┴─────────┘
                           │
                    合并所有 Layer
                    (OverlayFS mount)
                           │
                           ▼
                                   snapshot_dir
```

### 沙箱启动流程

```text
snapshot_dir
       │
          ▼ hardlink_copy
   base_dir/
       │
       ├── OverlayMount::new(lower=[base], upper, work, merged)
       │        │
       │        ▼ mount("overlay", ...)
       │   merged_dir/ ← 合并视图
       │
                      ├── clone(NEWUSER | NEWNS | NEWPID | NEWUTS | NEWIPC | NEWNET)
       │        │
                      │        ├── [子进程] setup /dev + bind /proc
                      │        │             pivot_root(merged_dir) → seccomp → exec(...)
       │        │
                      │        └── [父进程] setup_user_ns(pid) + cgroups + setup_sandbox_net(pid, index)
                      │                      uid_map/gid_map + veth pair + bridge + NAT
       │
       └── waitpid → unmount overlay → cleanup
```

### 隔离 Profile

```text
Build Profile
       - 用于 Dockerfile RUN
       - 保留 namespace 内 capabilities
       - 不设置 PR_SET_NO_NEW_PRIVS
       - 使用基础 seccomp 白名单

Runtime Profile
       - 用于 API start / CLI start 后台沙箱
       - 清空全部 capabilities
       - 设置 PR_SET_NO_NEW_PRIVS
       - 在基础 seccomp 白名单上额外移除 mount/pivot_root/ptrace/bpf/setns 等 syscall
       - 额外遮蔽 /proc 敏感路径，并将 /sys 挂成只读
```

## 目录结构

### 项目源码

```text
examples/moulin/
├── Cargo.toml               # 依赖定义
├── Dockerfile               # 项目自身容器化
├── src/
│   ├── main.rs              # CLI 入口 (clap)
│   ├── overlay.rs           # OverlayFS 挂载/卸载
│   ├── sandbox.rs           # Namespace + pivot_root + seccomp 沙箱
│   ├── snapshot.rs          # 快照目录管理 + tar.gz 辅助函数
│   ├── metadata.rs          # JSON 元数据持久化
│   ├── netns.rs             # veth 网络隔离
│   ├── seccomp_whitelist.rs # seccomp 白名单与 runtime denylist
│   ├── server.rs            # Axum HTTP API Server
│   └── build/
│       ├── mod.rs           # 构建引擎（驱动分层构建）
│       ├── parser.rs        # Dockerfile 指令解析器
│       └── registry.rs      # Docker Registry v2 客户端
└── images/                  # 示例 Dockerfile 集合
```

### 运行时数据 (`~/.moulin/`)

```text
~/.moulin/
├── metadata/
│   └── state.json           # 全局元数据（snapshot/sandbox 列表）
├── snapshots/               # 构建产物（目录型快照）
├── bases/                   # 从 Registry 拉取的基础镜像层
├── cache/                   # 构建指令缓存层（按 SHA256 索引）
└── sandboxes/               # 运行中的沙箱
    └── {uuid}/
        ├── base/            # 快照解压的只读层
        ├── upper/           # OverlayFS 可写层
        ├── work/            # OverlayFS 工作目录
        └── merged/          # OverlayFS 合并挂载点
```

## 依赖技术栈

| 依赖 | 版本 | 用途 |
|------|------|------|
| `nix` | 0.29 | Linux 系统调用封装 (clone, mount, pivot_root, unshare, etc.) |
| `clap` | 4.0 | CLI 参数解析 |
| `axum` | 0.8 | HTTP 服务框架 |
| `tokio` | 1.49 | 异步运行时 |
| `reqwest` | 0.12 | HTTP 客户端（Registry 交互） |
| `tar` | 0.4 | tar 归档操作 |
| `flate2` | 1.0 | gzip 压缩/解压 |
| `serde` / `serde_json` | 1.0 | JSON 序列化 |
| `sha2` | 0.10 | SHA256 哈希（构建缓存键） |
| `uuid` | 1.0 | UUID 生成 |
| `chrono` | 0.4 | 时间戳 |
| `fs_extra` | 1.3 | 高级文件系统操作 |
| `tracing` | 0.1 | 结构化日志 |
| `anyhow` | 1.0 | 错误处理 |
| `caps` | 0.5 | Linux capability 清理 |
| `libseccomp` | 0.3 | seccomp 过滤器 |
| `libc` | 0.2 | `prctl` 等底层系统调用 |

## 系统要求

- **操作系统**：Linux（内核 >= 4.0，需要 OverlayFS 支持）
- **权限**：root 或 `--privileged` 容器（需要挂载文件系统、创建 Namespace、操作 iptables / cgroups / user namespace 映射）
- **网络**：需要访问 Docker Registry（构建时拉取镜像）

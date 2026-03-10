# Dockerfile 构建与分层缓存机制

## 概述

mini-daytona-rs 实现了类似 Docker 的分层构建机制，核心特性包括：

- Dockerfile 指令解析
- Docker Registry v2 协议拉取基础镜像
- 基于 OverlayFS 的分层执行
- 基于 SHA256 的指令级缓存
- `RUN` 指令在 Build Profile 沙箱中执行，而不是直接在宿主机执行

## 构建引擎 (`build/mod.rs`)

### 构建流程

```text
build(dockerfile_path, context_dir) → snapshot_dir

  ┌─ parse_dockerfile(path) → Vec<Instruction>
  │
  ├─ FROM: pull_image(image_ref, layer_dir)
  │        └─ lower_dirs = [layer_dir]    ← 基础层
  │
  ├─ for each instruction:
  │    │
  │    ├─ 计算缓存键: SHA256(lower_dirs路径 + 指令文本)
  │    │
  │    ├─ [缓存命中] → 跳过执行，lower_dirs.push(cached_layer)
  │    │
  │    └─ [缓存未命中]:
  │         ├─ 创建 temp_dir/{upper, work, merged}
  │         ├─ OverlayMount::mount(lower_dirs, upper, work, merged)
  │         ├─ 执行指令:
  │         │    ├─ RUN  → run_sandbox(..., ["/bin/sh", "-c", cmd], SandboxProfile::Build)
  │         │    ├─ COPY → fs::copy(context/src, merged/dst)
  │         │    ├─ ENV  → 记录到 env 列表
  │         │    └─ ...
  │         ├─ OverlayMount::unmount(merged)
  │         ├─ 将 upper 移动/复制到 cache/{hash}
  │         └─ lower_dirs.push(cache_layer)
  │
  └─ 最终打包:
       ├─ OverlayMount::mount(all lower_dirs, ...)
      ├─ hardlink_copy(merged, snapshots/{uuid})
      └─ OverlayMount::unmount(merged)
```

    ### Build Profile

    `RUN` 指令由 `sandbox.rs` 以 Build Profile 执行，设计目标是“有隔离、但不误伤构建工具链”：

    - 使用 `CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWNET`
    - 父进程写入 `uid_map` / `gid_map`，让包管理器可以在 user namespace 内正常切换用户
    - 子进程在 OverlayFS 合并目录上执行 `pivot_root`
    - 保留 namespace 内 capabilities，兼容 `apt` / `apk` / `npm` / `pip`
    - 挂载 `/dev`、注入 `/proc`、配置网络和 cgroups
    - 使用基础 seccomp 白名单，但不施加 runtime denylist

    因此构建阶段仍然具备文件系统、进程、网络隔离，同时能兼容常见语言运行时和包管理器。

### 缓存键计算

```rust
fn compute_cache_key(lower_dirs: &[PathBuf], instruction: &str) -> String {
    let mut hasher = Sha256::new();
    for dir in lower_dirs {
        hasher.update(dir.to_str().unwrap().as_bytes());
    }
    hasher.update(instruction.as_bytes());
    format!("{:x}", hasher.finalize())
}
```

缓存键 = SHA256(所有 lower 层路径 + 指令文本)，确保：
- 相同基础层 + 相同指令 → 命中缓存
- 任何层变化都会导致后续所有层缓存失效

### 缓存存储

```text
~/.mini-daytona/cache/
├── a3f2b8c1...   ← SHA256 哈希作为目录名
│   └── (upper 层内容：仅包含该指令变更的文件)
├── 7d9e4f6a...
└── ...
```

---

## Dockerfile 解析器 (`build/parser.rs`)

### 支持的指令

| 指令 | 解析结果 | 说明 |
|------|---------|------|
| `FROM <image>` | `Instruction::From(String)` | 指定基础镜像 |
| `RUN <cmd>` | `Instruction::Run(String)` | 在 Build Profile 沙箱中执行 shell 命令 |
| `COPY <src> <dst>` | `Instruction::Copy { src, dst }` | 从 build context 复制文件 |
| `ADD <src> <dst>` | `Instruction::Add { src, dst }` | 同 COPY |
| `WORKDIR <dir>` | `Instruction::Workdir(String)` | 设置工作目录 |
| `ENV <key>=<value>` | `Instruction::Env { key, value }` | 设置环境变量 |
| `ENTRYPOINT [...]` | `Instruction::Entrypoint(Vec)` | 设置入口程序 |
| `CMD [...]` | `Instruction::Cmd(Vec)` | 设置默认命令 |
| `USER <user>` | `Instruction::User(String)` | 设置运行用户 |
| `EXPOSE <port>` | `Instruction::Expose(String)` | 声明端口 |

### 解析特性

- **注释处理**：忽略 `#` 开头的行
- **续行支持**：以 `\` 结尾的行会与下一行拼接
- **JSON 数组解析**：ENTRYPOINT 和 CMD 支持 `["arg1", "arg2"]` 格式
- **大小写不敏感**：指令关键字自动转大写匹配

---

## Docker Registry 客户端 (`build/registry.rs`)

### 协议实现

实现 Docker Registry HTTP API V2 的核心操作：

```text
1. GET /v2/<repo>/manifests/<tag>
   │
   ├── [401] → 解析 Www-Authenticate 头
   │           → GET <realm>?service=...&scope=...
   │           → 获取匿名 Bearer Token
   │           → 重试请求
   │
   ├── [manifest list] → 按平台筛选 (amd64/arm64)
   │                   → 再次请求具体 manifest
   │
   └── [manifest] → 获取 layers 列表

2. for each layer:
   GET /v2/<repo>/blobs/<digest>
   → 下载 tar.gz → 解压到 dest_dir
```

### 镜像引用解析

```text
输入                           →  registry          repo              tag
"alpine:3.19"                  →  docker.1ms.run    library/alpine    3.19
"alpine"                       →  docker.1ms.run    library/alpine    latest
"myregistry.io/myapp:v1"       →  myregistry.io     myapp             v1
"nginx/unit:latest"            →  docker.1ms.run    library/nginx/unit latest
```

### 镜像加速

- 默认使用镜像站 `docker.1ms.run` 替代 `registry-1.docker.io`
- 可通过 `DOCKER_MIRROR` 环境变量自定义镜像站

### 多架构支持

当获取到 manifest list 时，自动根据当前系统架构选择对应的 manifest：

| `std::env::consts::ARCH` | 目标架构 |
|--------------------------|---------|
| `aarch64` | `arm64` |
| `x86_64` | `amd64` |

### 认证流程

```text
GET /v2/<repo>/manifests/<tag>
  └── HTTP 401 Unauthorized
      └── Www-Authenticate: Bearer realm="...",service="...",scope="..."
          └── GET <realm>?service=<service>&scope=<scope>
              └── { "token": "eyJ..." }
                  └── 使用 Token 重试原始请求
```

- 支持匿名 Token 认证（无需用户名密码）
- Token 自动缓存并复用于后续请求
- 请求超时设置为 300 秒（适应大层下载）

---

## 分层构建示例

以下 Dockerfile 的构建过程展示了分层机制：

```dockerfile
FROM alpine:3.19
RUN apk add --no-cache python3
COPY app.py /app/
ENV APP_ENV=production
```

### 首次构建

```text
Step 1/4: FROM alpine:3.19
  → pull_image() → 下载并解压 3 个 layers → bases/{uuid}/
  → lower_dirs = [bases/{uuid}]

Step 2/4: RUN apk add --no-cache python3
  → cache_key = SHA256(bases/{uuid} + "RUN apk add --no-cache python3")
  → 缓存未命中
  → overlay mount(lower=[bases/{uuid}], upper=tmp/upper)
  → run_sandbox(..., ["/bin/sh", "-c", "apk add --no-cache python3"], SandboxProfile::Build)
  → unmount → 移动 upper 到 cache/{hash}
  → lower_dirs = [bases/{uuid}, cache/{hash}]

Step 3/4: COPY app.py /app/
  → cache_key = SHA256(bases/{uuid} + cache/{hash} + "COPY app.py /app/")
  → 缓存未命中
  → overlay mount → fs::copy → unmount → 存入缓存
  → lower_dirs = [bases/{uuid}, cache/{hash}, cache/{hash2}]

Step 4/4: ENV APP_ENV=production
  → 仅记录元数据，不产生新层

Final: overlay mount(all 3 layers) → hardlink_copy → snapshot_dir
```

### 第二次构建（缓存命中）

```text
Step 1/4: FROM alpine:3.19  → 重新拉取（可优化）
Step 2/4: RUN ...            → "Using cache" ← 命中
Step 3/4: COPY ...           → "Using cache" ← 命中（如果 app.py 未变）
Final: 快速生成 snapshot
```

---

## 与 Docker 的差异

| 特性 | Docker | mini-daytona-rs |
|------|--------|-----------------|
| 层存储 | content-addressable | 路径 SHA256 |
| 镜像缓存 | 按内容哈希 | 按路径+指令哈希 |
| 多阶段构建 | 支持 | 不支持 |
| 层压缩 | 单独存储每层 | 最终合并打包 |
| ARG/变量替换 | 支持 | 不支持 |
| 健康检查 | HEALTHCHECK | 不支持 |
| 构建上下文 | .dockerignore | 全量复制 |
| 安全 | rootless 可选 | Build/Runtime 双 profile，当前仍要求 root 或 privileged 容器 |

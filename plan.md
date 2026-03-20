# moulin：极简容器运行时（Rust, Linux）

用 Rust 从零构建极简容器运行时。直接调用 Linux 内核 API 实现隔离，支持 **Dockerfile 构建** snapshot。本次更新全面引入 **OverlayFS**，不仅用于沙箱运行时的隔离，还用于实现类似 Docker 的**分层构建（Layered Build）与缓存机制**。

> [!CAUTION]
> 仅限 Linux，需要 root 权限。使用 Linux Namespaces、chroot、OverlayFS。

## 核心架构（新增分层构建支持）

```text
                    ┌─────────────────────┐
                    │     Dockerfile      │
                    │  FROM alpine:3.19   │
                    │  RUN apk add python3│
                    │  COPY app.py /app/  │
                    └────────┬────────────┘
                             │ build (利用 OverlayFS 实现分层)
                             ▼
┌────────────────────────────────────────────────────────────┐
│  1. Docker Registry Client 提取 base image 为 Layer 0       │
│  2. RUN 指令: Mount Layer 0 (Lower) + 新 Upper 层            │
│     → chroot 执行 → 将 Upper 固化为 Layer 1                  │
│  3. COPY 指令: Mount Layer 1 (Lower) + 新 Upper 层           │
│     → 复制文件 → 将 Upper 固化为 Layer 2                     │
│  4. 最终将所有合并视图打包为 Snapshot (tar.gz)                │
└────────────────────┬───────────────────────────────────────┘
                     │
                     ▼
              Snapshot (tar.gz)     ← 也可以手动从目录创建
                     │
                     │ start (extract → OverlayFS)
                     ▼
┌────────────────────────────────────────┐
│  Sandbox                               │
│  ┌─────────┐ ┌─────────┐               │
│  │ Lower   │ │ Upper   │  OverlayFS    │
│  │(只读base)│ │(可写层) │  → merged/    │
│  └─────────┘ └─────────┘               │
│  + PID Namespace + Mount NS + UTS NS   │
│  + chroot(merged/) + /proc mount       │
│  + exec /bin/sh                        │
└────────────────────────────────────────┘

```

## 项目结构

```text
examples/moulin/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI 入口
│   ├── snapshot.rs          # tar.gz 打包/解压，以及层(Layer)的管理
│   ├── sandbox.rs           # Namespaces + chroot + 进程管理
│   ├── overlay.rs           # OverlayFS 挂载/卸载，支持多层 LowerDir 组合
│   ├── build/
│   │   ├── mod.rs           # 构建引擎（按指令执行 Dockerfile，驱动 OverlayFS 缓存）
│   │   ├── parser.rs        # Dockerfile 解析器
│   │   └── registry.rs      # Docker Registry v2 客户端（拉取 base image）
│   └── metadata.rs          # snapshot/sandbox/build_cache 元数据管理
└── README.md

```

## 依赖 (Cargo.toml)

```toml
[dependencies]
nix = { version = "0.29", features = ["sched", "unistd", "mount", "fs"] }
tar = "0.4"
flate2 = "1.0"
uuid = { version = "1", features = ["v4"] }
reqwest = { version = "0.12", features = ["blocking", "json"] }  # Registry API
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10" # [NEW] 用于计算 Dockerfile 指令的 Hash，实现层缓存

```

## CLI 命令

| 命令 | 说明 |
| --- | --- |
| `build <Dockerfile> [context_dir]` | 解析 Dockerfile → 拉取 base → 在 OverlayFS + chroot 中逐层执行 → 输出 snapshot |
| `start <snapshot.tar.gz>` | 解压 → OverlayFS → namespace → chroot → /bin/sh |
| `snapshot <sandbox_id> <output.tar.gz>` | 打包 merged 为新 snapshot |
| `list` | 列出 sandbox |
| `destroy <sandbox_id>` | 卸载 OverlayFS + 清理 |

---

## 模块设计

### `build/parser.rs` — Dockerfile 解析器

支持的指令（最小集）：

| 指令 | 行为 |
| --- | --- |
| `FROM <image>` | 从 Registry 拉取 base image，或引用本地 snapshot |
| `RUN <cmd>` | 在 chroot 环境中执行 shell 命令 |
| `COPY <src> <dst>` | 从 build context 复制文件到 rootfs |
| `ADD <src> <dst>` | 同 COPY（简化实现） |
| `WORKDIR <dir>` | 设置后续 RUN/COPY 的工作目录 |
| `ENV <key>=<value>` | 设置环境变量 |
| `ENTRYPOINT [...]` | 设置 snapshot 默认入口 |
| `CMD [...]` | 设置默认参数 |

解析结果为 `Vec<Instruction>` 枚举。

### `build/registry.rs` — Docker Registry v2 客户端

实现从 Docker Hub 等 Registry 拉取 base image：

1. **解析镜像引用** — `alpine:3.19` → `registry-1.docker.io/library/alpine:3.19`
2. **获取 Token** — `GET https://auth.docker.io/token?service=...&scope=...`
3. **获取 Manifest** — `GET /v2/<repo>/manifests/<tag>` (Accept: `application/vnd.oci.image.manifest.v1+json`)
4. **下载 Layers** — `GET /v2/<repo>/blobs/<digest>` → tar.gz blobs
5. **解压 Layers** — 按顺序解压所有 layer 到 rootfs 目录 构成完整文件系统

### `build/mod.rs` — 基于分层的构建引擎（大幅重构）

```text
build(dockerfile_path, context_dir) → snapshot.tar.gz:
  1. parse(dockerfile) → instructions[]
  2. FROM: registry.pull(image) → 提取为 Layer 0 目录
  3. 当前 Lower 层 = Layer 0
  4. for each instruction:
     - 计算当前指令的 Hash: `hash(Lower 层 Hash + Instruction 文本)`
     - 检查缓存: 若 `~/.moulin/cache/<Hash>` 存在，则跳过执行，更新 Lower 层 = 该缓存层。
     - 若无缓存:
         - 创建工作区 `UpperDir` 和 `WorkDir`。
         - overlay.mount_overlay(Lower 层, UpperDir, WorkDir, MergedDir)
         - 根据指令类型修改 MergedDir：
             - COPY/ADD: cp from context_dir to MergedDir
             - ENV: 记录环境变量 (更新 metadata)
             - WORKDIR: 记录工作目录 (更新 metadata)
             - RUN: unshare(NEWNS|NEWPID) → fork → chroot(MergedDir) → exec(cmd)
             - ENTRYPOINT/CMD: 记录元数据
         - overlay.unmount_overlay(MergedDir)
         - 将 `UpperDir` 移动并重命名为 `~/.moulin/cache/<Hash>`，作为新的 Layer。
         - 更新 Lower 层为新计算出的组合视图。
  5. 遍历结束后，将最终的视图层结构打包：tar.gz(最终组合视图) → snapshot file

```

### `snapshot.rs`

* `create_archive(source_dir, output_path)` / `extract_archive(archive_path, dest_dir)`
* `extract_snapshot_base(snapshot_path)` → `~/.moulin/bases/{hash}/`

### `overlay.rs`（增强支持多层）

* `mount_overlay(lower_dirs: Vec<String>, upper_dir, work_dir, merged_dir)` → 拼接 `lowerdir=l1:l2:l3,upperdir=u,workdir=w` 并执行挂载。
* `unmount_overlay(merged_dir)` → umount + cleanup
* 运行时沙箱布局：`~/.moulin/sandboxes/{id}/{upper,work,merged}/`

### `sandbox.rs`

* `run_sandbox(merged_dir)` → unshare → fork → chroot → mount /proc → exec /bin/sh
* 父进程 waitpid

### `metadata.rs`

* JSON 文件记录 snapshot 列表、sandbox 列表、entrypoint/env 以及 Layer 缓存映射等元数据
* 存储于 `~/.moulin/metadata/`

---

## Verification Plan

```bash
# 编译
cargo build --release

# === 方式 1: 从 Dockerfile 构建 ===
cat > /tmp/Dockerfile <<EOF
FROM alpine:3.19
RUN apk add --no-cache python3
COPY app.py /app/
ENV APP_ENV=production
ENTRYPOINT ["python3", "/app/app.py"]
EOF
echo 'print("Hello from moulin!")' > /tmp/app.py

sudo ./target/release/moulin build /tmp/Dockerfile /tmp/
# → 输出 snapshot: ~/.moulin/snapshots/xxx.tar.gz

# 重复执行上述 build 命令，观察输出，应当显示 "Using cache" 而瞬间完成，验证分层缓存生效。

# === 方式 2: 从现有目录创建 ===
sudo ./target/release/moulin snapshot ./my-rootfs output.tar.gz

# === 启动 sandbox ===
sudo ./target/release/moulin start <snapshot.tar.gz>
# 进入隔离环境, 验证: python3 --version, ls /app/

# === 验证 OverlayFS 隔离 ===
# 启动两个 sandbox, 在一个中修改文件, 另一个不受影响

```

你需要我们先从 `build/registry.rs`（实现基础镜像的拉取）开始编写代码，还是先实现底层的 `overlay.rs`（挂载逻辑）？
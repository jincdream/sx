# 模块详细实现说明

## 1. `overlay.rs` — OverlayFS 文件系统

### 职责

管理 OverlayFS 的挂载与卸载，为构建和运行时提供分层文件系统支持。

### 核心结构体

```rust
pub struct OverlayMount {
    pub lower_dirs: Vec<PathBuf>,  // 只读层（可多个）
    pub upper_dir: PathBuf,        // 可写层
    pub work_dir: PathBuf,         // OverlayFS 工作目录
    pub merged_dir: PathBuf,       // 合并挂载点
}
```

### 关键方法

| 方法 | 功能 |
|------|------|
| `new()` | 构造 OverlayMount 实例 |
| `mount()` | 创建目录并执行 `mount("overlay", ...)` 系统调用 |
| `unmount()` | 执行 `umount` 卸载合并目录 |
| `cleanup()` | 清理所有临时目录 |

### 挂载逻辑

`mount()` 方法构建 OverlayFS 挂载选项字符串：

```text
lowerdir=<layer1>:<layer2>:...,upperdir=<upper>,workdir=<work>
```

- **多层 lower**：多个只读层用 `:` 分隔，实现分层叠加
- **空 lower 处理**：当 `lower_dirs` 为空时，自动创建一个空的 `empty_lower` 目录作为占位
- **系统调用**：最终调用 `nix::mount::mount()` 执行内核 OverlayFS 挂载

### OverlayFS 原理

```text
┌─────────── merged (用户可见的合并视图) ───────────┐
│  文件来自 upper（优先）+ lower 层叠加              │
├──────────────────────────────────────────────────┤
│  upper/  ── 可写层，所有修改写入此处               │
│  lower/  ── 只读层，原始数据                      │
│  work/   ── OverlayFS 内部使用的工作目录           │
└──────────────────────────────────────────────────┘
```

- 读取文件：从 upper 到 lower 逐层查找
- 写入/修改：写时复制（CoW），数据写入 upper 层
- 删除文件：在 upper 层创建 whiteout 文件

---

## 2. `sandbox.rs` — 容器沙箱

### 职责

通过 Linux namespace、`pivot_root`、seccomp 和 cgroups 创建隔离的进程执行环境。

### 隔离机制

使用 `clone()` 系统调用创建子进程，启用以下 Namespace：

| Namespace | CloneFlag | 隔离内容 |
|-----------|-----------|---------|
| Mount NS | `CLONE_NEWNS` | 文件系统挂载点 |
| User NS | `CLONE_NEWUSER` | UID/GID 映射与 namespace 内特权边界 |
| PID NS | `CLONE_NEWPID` | 进程 ID 空间 |
| UTS NS | `CLONE_NEWUTS` | 主机名 |
| IPC NS | `CLONE_NEWIPC` | 进程间通信 |
| Network NS | `CLONE_NEWNET` | 网络栈 |

### 执行流程

```text
run_sandbox(sandbox_id, merged_dir, cmd, limits, workdir, profile)
  │
  ├── clone(flags=NEWUSER|NEWNS|NEWPID|NEWUTS|NEWIPC|NEWNET, signal=SIGCHLD)
  │     │
  │     ├── [子进程] child()
  │     │     ├── sethostname("moulin")
  │     │     ├── mount(MS_SLAVE | MS_REC)     // 防止挂载向宿主传播
  │     │     ├── setup_dev(new_root)
  │     │     ├── bind-mount /proc into new root
  │     │     ├── pivot_root(new_root)
  │     │     ├── chdir("/")
  │     │     ├── mask_proc(profile) / mask_sys(runtime)
  │     │     ├── setup_seccomp(profile)
  │     │     └── execvp(cmd 或 /bin/sh)
  │     │
  │     └── [父进程]
  │           ├── setup_user_ns(pid)           // 写入 uid_map/gid_map
  │           ├── setup_cgroups(sandbox_id)    // memory/cpu/pids
  │           ├── allocate_index()             // 分配网络 IP 索引
  │           ├── setup_sandbox_net(pid, idx)  // 配置 veth 网络
  │           ├── 通过 pipe 唤醒子进程         // 替代固定 sleep
  │           ├── waitpid(child)               // 等待子进程退出
  │           └── teardown_sandbox_net(idx)    // 清理网络
  │
  └── return Ok(())
```

### 关键实现细节

- **栈空间**：子进程使用 1MB 独立栈（`STACK_SIZE = 1024 * 1024`）
- **父子同步**：使用 pipe 阻塞/唤醒子进程，等待 user namespace、cgroups 和网络配置完成
- **进程替换**：使用 `execvp` 替换子进程，默认执行 `/bin/sh`
- **运行时硬化**：Runtime Profile 下清空全部 capabilities，设置 `PR_SET_NO_NEW_PRIVS`，并在 seccomp 白名单基础上追加 denylist
- **兼容性设计**：Build Profile 保留 namespace 内 capabilities，避免破坏 `apt` / `npm` / `pip` 等安装流程

### 安全 Profile

| Profile | 主要用途 | Capabilities | `PR_SET_NO_NEW_PRIVS` | Seccomp |
|---------|---------|--------------|------------------------|---------|
| `Build` | Dockerfile `RUN` | 保留 namespace 内 capability | 不设置 | 基础白名单 |
| `Runtime` | API / CLI 启动后的后台沙箱 | 全部清空 | 设置 | 基础白名单减去 runtime denylist |

---

## 3. `snapshot.rs` — 快照管理

### 职责

管理目录型快照、tar.gz 辅助打包能力，以及运行时目录结构。

### 核心函数

| 函数 | 功能 |
|------|------|
| `hardlink_copy(src, dst)` | 递归硬链接复制目录树，构建目录型快照 |
| `create_archive(source, output)` | 将目录打包为 tar.gz |
| `extract_archive(archive, dest)` | 解压 tar.gz 到目标目录 |
| `get_data_dir()` | 返回 `~/.moulin/` |
| `get_snapshots_dir()` | 返回 `~/.moulin/snapshots/` |
| `get_bases_dir()` | 返回 `~/.moulin/bases/` |
| `get_cache_dir()` | 返回 `~/.moulin/cache/` |
| `get_sandboxes_dir()` | 返回 `~/.moulin/sandboxes/` |

### 打包实现

当前主构建路径默认产出的是**目录型快照**，而不是 tar.gz：

- 构建完成后把最终 OverlayFS 合并视图通过 `hardlink_copy()` 复制到 `snapshots/{uuid}`
- 同盘场景优先使用硬链接，速度远快于 tar/gzip 压缩
- `create_archive()` / `extract_archive()` 仍保留，用于 CLI `snapshot` 等归档场景

```rust
create_archive:
  File::create(output)
  → GzEncoder::new(file, Compression::default())
  → Builder::new(encoder)
  → tar.append_dir_all(".", source_dir)
  → tar.finish()
```

- 使用 `flate2` 进行 gzip 压缩
- 使用 `tar` crate 创建 tar 归档
- `follow_symlinks(false)` 保持符号链接原样

---

## 4. `metadata.rs` — 元数据管理

### 职责

持久化存储 snapshot 和 sandbox 的元数据信息。

### 数据结构

```rust
struct Metadata {
    snapshots: HashMap<String, SnapshotMetadata>,
    sandboxes: HashMap<String, SandboxMetadata>,
}

struct SnapshotMetadata {
    id: String,
    path: PathBuf,
    created_at: String,         // RFC 3339 时间戳
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Option<Vec<String>>,
}

struct SandboxMetadata {
    id: String,
    snapshot_id: String,
    created_at: String,
    dir: PathBuf,
  pid: Option<i32>,
}
```

### 存储

- **格式**：JSON（`serde_json` 序列化）
- **路径**：`~/.moulin/metadata/state.json`
- **策略**：
  - `load_metadata()`：文件不存在时返回空的默认 `Metadata`
  - `save_metadata()`：使用 `to_string_pretty` 生成可读 JSON

---

## 5. `netns.rs` — 网络命名空间

### 职责

为每个沙箱配置独立的网络栈，实现容器间网络隔离和外网访问。

### 网络架构

```text
          Host
    ┌──────────────────────┐
    │   moulin0 (bridge)  │  10.200.0.1/24
    │     │          │     │
    │  veth-h-2   veth-h-3 │  ...
    └─────┼──────────┼─────┘
          │          │
   ───────┼──────────┼─────── Namespace 边界
          │          │
    ┌─────┼────┐ ┌───┼──────┐
    │  eth0    │ │  eth0    │
    │10.200.  │ │10.200.  │
    │   0.2   │ │   0.3   │
    │ Sandbox1│ │ Sandbox2│
    └─────────┘ └──────────┘
```

### 配置步骤

1. **创建网桥** (`ensure_bridge`)：
   - `ip link add moulin0 type bridge`
   - 分配 IP `10.200.0.1/24`
   - 启用 IP 转发 (`/proc/sys/net/ipv4/ip_forward`)
   - 配置 iptables MASQUERADE + FORWARD 规则

2. **配置沙箱网络** (`setup_sandbox_net`)：
   - 创建 veth pair：`veth-h-{idx}` ↔ `veth-s-{idx}`
   - 将 host 端加入网桥
   - 将 sandbox 端移入子进程的 Network Namespace
   - 在 sandbox 内配置 IP、路由、DNS

3. **清理** (`teardown_sandbox_net`)：
   - 删除 host 端 veth（自动删除 peer）
   - 释放 IP 索引

### IP 分配

- 使用 `AtomicU8` 原子计数器分配 IP 索引（2–254）
- 子网：`10.200.0.0/24`，网关：`10.200.0.1`
- 最多支持 253 个并发沙箱

---

## 6. `main.rs` — CLI 入口

### 职责

解析 CLI 命令，调度各模块完成具体操作。

### 命令体系

```text
moulin [OPTIONS] <COMMAND>

OPTIONS:
  -l, --log-level <LEVEL>    设置日志级别

COMMANDS:
  build <dockerfile> [context]    从 Dockerfile 构建快照
  start <snapshot>                从快照启动沙箱
  snapshot <id> <output>          将沙箱打包为快照
  list                            列出 snapshot 和 sandbox
  destroy <id>                    销毁沙箱
  server                          启动 HTTP API 服务
```

### 日志系统

使用 `tracing` + `tracing-subscriber`：
- 支持 `RUST_LOG` 环境变量或 `--log-level` 参数
- 默认级别为 `info`
- 输出格式：去除线程 ID 和 target，保持简洁

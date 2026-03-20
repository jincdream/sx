# 安全设计与隔离模型

## 概述

moulin 当前采用“双 profile”沙箱设计：

- `Build Profile`：用于 Dockerfile `RUN`
- `Runtime Profile`：用于 API/CLI 启动后的后台沙箱，以及后续 `exec`

这两个 profile 共享同一套基础隔离骨架：

- User namespace
- Mount namespace
- PID namespace
- UTS namespace
- IPC namespace
- Network namespace
- OverlayFS 根文件系统隔离
- cgroups v2 资源限制
- seccomp 系统调用过滤

它们的区别不在于“是否隔离”，而在于“隔离收紧到什么程度”。Build 需要兼容包管理器和镜像构建脚本，Runtime 则优先减少攻击面。

## 威胁模型

当前设计主要防御以下风险：

- 沙箱进程直接读写宿主机文件系统
- 沙箱进程观察或干扰宿主机/其他沙箱的进程、挂载点、主机名、IPC 资源
- 通过危险系统调用扩大内核攻击面
- 运行时进程通过 setuid / file capabilities 获得额外特权
- 沙箱进程无限制消耗内存、CPU、PIDs

当前设计**不**完全防御以下风险：

- Linux 内核 0-day 或 namespace / seccomp / OverlayFS 内核漏洞
- 同一 bridge 上沙箱之间的横向网络访问
- 高强度多租户对抗场景下的侧信道攻击
- 需要设备直通、GPU 隔离、LSM 强策略的生产级硬隔离需求

## 基础隔离骨架

### 1. User Namespace

沙箱通过 `CLONE_NEWUSER` 创建新的 user namespace，随后父进程写入：

```text
uid_map: 0 0 65536
gid_map: 0 0 65536
setgroups: allow
```

效果：

- 沙箱内的 `root` 只在 namespace 内拥有特权
- 这些 capability 不会穿透回宿主机 init user namespace
- 构建阶段的 `apt` / `npm` / `pip` 等可以在 namespace 内正常执行降权/切用户动作

### 2. 文件系统隔离

沙箱根文件系统来自 OverlayFS 合并视图：

```text
lower dirs  ->  只读层
upper dir   ->  可写层
work dir    ->  OverlayFS 工作目录
merged dir  ->  作为新 root 使用
```

子进程会：

- 将 `merged_dir` bind mount 到自身
- 预先在新根下准备 `/dev`、`/proc`
- 使用 `pivot_root` 切换根文件系统
- 卸载旧 root

相对于简单 `chroot`，`pivot_root` 更接近 OCI/runc 的行为，能更彻底地切断旧根暴露。

### 3. 进程与系统视图隔离

启用以下 namespace：

- `CLONE_NEWPID`
- `CLONE_NEWNS`
- `CLONE_NEWUTS`
- `CLONE_NEWIPC`
- `CLONE_NEWNET`
- `CLONE_NEWUSER`

另外，在 `/proc` 可用后会再尝试创建 `CLONE_NEWCGROUP`，减少沙箱看到的宿主 cgroup 视图。

### 4. 资源限制

通过 cgroups v2 为每个沙箱创建独立 cgroup：

- `memory.max`
- `pids.max`
- `cpu.max`

默认值：

- 内存：`1 GiB`
- CPU：`100000/100000`，即约 1 核
- PIDs：`512`

### 5. 网络隔离

每个沙箱拥有独立 network namespace，通过 veth pair 接入 `moulin0` bridge，并使用 iptables NAT 访问外网。

这保证了：

- 沙箱拥有独立 `eth0`、路由表和回环设备
- 宿主机只暴露 bridge 侧地址
- 沙箱可以访问外网，满足构建和运行常见语言运行时的需求

## Build / Runtime Profile 对比

| 维度 | Build Profile | Runtime Profile |
|------|---------------|----------------|
| 主要用途 | Dockerfile `RUN` | 长生命周期运行沙箱、`exec` |
| Capabilities | 保留 namespace 内 capability | 全部清空 |
| `PR_SET_NO_NEW_PRIVS` | 不设置 | 设置 |
| Seccomp | 基础白名单 | 基础白名单减去 runtime denylist |
| `/proc` | 基础遮蔽 | 扩展遮蔽 + 更多只读处理 |
| `/sys` | 默认保持可访问 | 只读 remount |
| 兼容目标 | apt/apk/npm/pip 等安装流程 | Node/Python/Chromium 等运行时 |

## Runtime Profile 的额外硬化

### Capability 清理

Runtime profile 会清空：

- Bounding set
- Effective set
- Inheritable set
- Permitted set

效果：

- 即使进程本身尝试做更高权限动作，也缺少 capability 支撑
- 可以显著减少 mount、内核参数、设备、raw socket 等高风险面的可达性

### `PR_SET_NO_NEW_PRIVS`

在 Runtime profile 中会调用：

```text
prctl(PR_SET_NO_NEW_PRIVS, 1)
```

效果：

- 后续 `execve()` 不会因为 setuid / file capabilities 获得额外特权
- seccomp 过滤器更符合“不可放宽”的防线预期

### `/proc` 与 `/sys` 保护

基础 `/proc` 遮蔽包括：

- `/proc/kcore`
- `/proc/sched_debug`
- `/proc/sysrq-trigger`
- `/proc/timer_list`
- `/proc/timer_stats`

Runtime profile 额外处理：

- 遮蔽 `/proc/acpi`
- 遮蔽 `/proc/keys`
- 遮蔽 `/proc/latency_stats`
- 遮蔽 `/proc/scsi`
- 将 `/proc/bus`、`/proc/fs`、`/proc/irq` 只读 remount
- 将 `/proc/sys` 只读 remount
- 将 `/sys` 只读 remount

### Runtime Denylist

Runtime profile 在基础 seccomp 白名单上进一步移除高风险 syscall，包括：

- 挂载与根切换：`mount`、`umount`、`umount2`、`pivot_root`、`chroot`
- 命名空间逃逸/扩展：`unshare`、`setns`
- 调试与跨进程访问：`ptrace`、`process_vm_readv`、`process_vm_writev`
- 高风险内核接口：`bpf`、`userfaultfd`
- 模块/重启相关：`init_module`、`finit_module`、`delete_module`、`reboot`、`kexec_load`
- capability 与 keyring：`capset`、`add_key`、`keyctl`、`request_key`

## 为什么不把 Runtime 限制直接用于 Build

如果直接把 Runtime 级别的约束套到构建阶段，会显著破坏兼容性：

- 包管理器经常需要切用户、切组、临时提权或调用更宽的系统调用集合
- Chromium/Node/Python 运行时与构建脚本对 syscall 面和进程模型的需求不同
- 构建阶段还会做更多文件系统和安装动作，过早清 capability 容易导致构建失败

因此当前策略是：

- Build: 兼顾隔离与兼容性
- Runtime: 面向执行期防御收紧攻击面

## 与常见运行时的兼容性

当前已经验证 Runtime profile 下可正常运行：

- Nginx 静态文件环境
- Python 脚本执行
- Pandas / Excel 数据处理
- 文件上传 / 下载
- 资源限制测试
- Puppeteer + Chromium

已验证命令：

```bash
cd examples/moulin && node test/run_e2e.js --client
```

## 当前已知边界

### 1. 仍然依赖 privileged 宿主环境

当前实现通常运行在 root 或 `--privileged` 容器中，因为需要：

- 创建 namespace
- 管理 bridge / veth / iptables
- 挂载 OverlayFS
- 操作 cgroups v2

### 2. 网络仍偏“功能优先”

当前所有沙箱接入同一 `moulin0` bridge：

- 默认允许沙箱之间互通
- 尚未实现 egress 白名单
- 尚未实现端口级 ACL
- 尚未实现每租户/每沙箱独立 bridge 或 VLAN

### 3. 尚未接入 LSM 强化

当前没有接入：

- AppArmor
- SELinux
- Landlock

这意味着隔离主要依赖 namespace + seccomp + cgroups，而非额外的内核强制访问控制层。

### 4. seccomp 仍是静态规则集

当前 seccomp 使用静态白名单 + runtime denylist，没有按进程类型或工作负载动态裁剪 syscall 面。

## 后续强化方向

如果继续向更强的生产级隔离演进，优先级建议如下：

1. 为 Runtime profile 增加网络 egress 控制与沙箱间隔离
2. 引入 AppArmor / SELinux / Landlock 作为额外强制访问控制层
3. 进一步拆分 seccomp profile，例如 shell / chromium / python / generic runtime
4. 改进 IP 回收和多租户网络拓扑
5. 为设备、GPU、共享内存、tmpfs 大小等增加更细粒度的策略配置

## 总结

当前 moulin 的安全模型已经从“单一宽松沙箱”演进到“Build/Runtime 双 profile 沙箱”：

- Build profile 保证构建成功率和生态兼容性
- Runtime profile 面向执行期，显著收紧 capability、syscall、`/proc`、`/sys` 暴露面
- 在保持 Chromium、Node、Python 正常运行的前提下，已经具备接近轻量生产容器运行时的基础防线

如果目标是强多租户、面向不可信代码的大规模生产环境，仍建议继续叠加 LSM、网络策略和更细粒度 seccomp。
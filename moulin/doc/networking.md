# 网络隔离实现

## 概述

moulin 通过 Linux Network Namespace + veth pair + 网桥 + iptables NAT 为每个沙箱提供独立的网络环境，同时支持沙箱访问外部网络。

网络配置由父进程完成，发生在 `clone()` 之后、子进程真正进入业务命令之前。`sandbox.rs` 通过 pipe 阻塞子进程，等待 user namespace、cgroups 和网络都准备好，再唤醒子进程继续执行。

该网络模型同时服务于两类沙箱：

- `Build Profile`：用于 Dockerfile `RUN`，让 `apt` / `apk` / `npm` / `pip` 能访问外部网络
- `Runtime Profile`：用于 API/CLI 启动后的后台沙箱，供 Node / Python / Chromium 等运行时访问网络

## 网络拓扑

```text
                         ┌──── Internet ────┐
                         │                  │
                    ┌────┴──── Host ────────┴────┐
                    │     eth0 / wlan0            │
                    │         │                   │
                    │    iptables NAT             │
                    │    (MASQUERADE)             │
                    │         │                   │
                    │  ┌──────┴──────────┐        │
                    │  │   moulin0      │        │
                    │  │   (bridge)      │        │
                    │  │  10.200.0.1/24  │        │
                    │  └──┬─────────┬────┘        │
                    │     │         │              │
                    │  veth-h-2  veth-h-3  ...    │
                    └─────┼─────────┼─────────────┘
                          │         │
                    ══════╪═════════╪═══════ Namespace 边界
                          │         │
                    ┌─────┼───┐ ┌───┼───────┐
                    │   eth0  │ │  eth0     │
                    │ .0.2/24 │ │ .0.3/24  │
                    │         │ │           │
                    │  gw:    │ │  gw:     │
                    │  .0.1   │ │  .0.1    │
                    │Sandbox 1│ │Sandbox 2 │
                    └─────────┘ └───────────┘
```

## 实现细节

### 1. 网桥初始化 (`ensure_bridge`)

在 API Server 启动或首次创建沙箱时调用，幂等操作：

```bash
# 创建网桥
ip link add moulin0 type bridge
ip addr add 10.200.0.1/24 dev moulin0
ip link set moulin0 up

# 启用 IP 转发
echo 1 > /proc/sys/net/ipv4/ip_forward

# iptables NAT — 允许沙箱访问外部网络
iptables -t nat -A POSTROUTING -s 10.200.0.0/24 -j MASQUERADE

# iptables FORWARD — 允许桥接子网收发流量
iptables -A FORWARD -s 10.200.0.0/24 -j ACCEPT
iptables -A FORWARD -d 10.200.0.0/24 -j ACCEPT
```

实现细节：

- 实际代码会先用 `iptables -C` 检查规则是否存在，再决定是否追加，避免重复插入
- `moulin0` 网桥地址固定为 `10.200.0.1/24`
- 需要可写 `/proc/sys/net/ipv4/ip_forward` 与 iptables 权限，因此通常要求 root 或 privileged 容器环境

### 2. 沙箱网络配置 (`setup_sandbox_net`)

在 `clone()` 创建子进程后，由父进程执行：

```bash
# 创建 veth pair
ip link add veth-h-{idx} type veth peer name veth-s-{idx}

# host 端加入网桥
ip link set veth-h-{idx} master moulin0
ip link set veth-h-{idx} up

# sandbox 端移入子进程的 Network Namespace
ip link set veth-s-{idx} netns {child_pid}

# 在 sandbox 内配置网络（通过 nsenter）
nsenter -t {pid} -n ip link set lo up
nsenter -t {pid} -n ip link set veth-s-{idx} name eth0
nsenter -t {pid} -n ip addr add 10.200.0.{idx}/24 dev eth0
nsenter -t {pid} -n ip link set eth0 up
nsenter -t {pid} -n ip route add default via 10.200.0.1
```

在 `sandbox.rs` 中，父进程完成上述步骤后才会通过 pipe 向子进程发送继续信号，因此业务进程看到的网络通常已经可用，不再依赖固定 `sleep`。

### 3. DNS 配置

将宿主机的 `/etc/resolv.conf` 复制到沙箱 rootfs 的 `etc/resolv.conf`：

```rust
fs::copy("/etc/resolv.conf", merged_dir/etc/resolv.conf)
```

说明：

- 该操作在父进程侧完成，目标是 OverlayFS 的 `merged_dir`
- 构建缓存层在写回前会移除该文件，避免把宿主机 DNS 配置固化进构建产物
- Runtime 沙箱保留该文件，用于正常域名解析

### 4. 清理 (`teardown_sandbox_net`)

```bash
# 删除 host 端 veth（peer 端自动删除）
ip link del veth-h-{idx}
```

## IP 分配机制

- 使用 `AtomicU8` 原子计数器，从 2 开始递增分配
- 地址范围：`10.200.0.2` ~ `10.200.0.254`
- 最大并发沙箱数：**253**
- **注意**：当前实现不回收 IP 索引（IP 池用尽后需重启服务）

## 安全考虑

| 方面 | 实现 |
|------|------|
| 网络隔离 | 每个沙箱拥有独立的 Network Namespace |
| 外网访问 | 通过 NAT 转发，沙箱 IP 不对外暴露 |
| 沙箱间通信 | 当前同网段可互通，尚未加细粒度 east-west 访问控制 |
| 命令执行 | 使用 `nsenter` 进入目标 Namespace 执行网络配置 |
| 错误处理 | 网络配置失败会记录 warning 但不阻断沙箱启动 |

当前限制：

- IP 索引只增不回收，长时间运行后可能耗尽 `10.200.0.2` ~ `10.200.0.254`
- 所有沙箱共享同一二层桥，默认允许沙箱之间互通
- 还没有为 Runtime Profile 单独增加 egress 白名单、端口级 ACL 或每沙箱独立 bridge/VLAN

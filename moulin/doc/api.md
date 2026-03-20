# HTTP API 接口文档

## 概述

moulin 提供基于 Axum 的 REST API，监听在 `0.0.0.0:3000`。

通过 `moulin server` 命令启动 API 服务。

## 通用响应格式

所有 API 返回统一的 JSON 格式：

```json
{
  "success": true,
  "data": { ... },
  "error": null
}
```

失败时：

```json
{
  "success": false,
  "data": null,
  "error": "错误描述信息"
}
```

---

## API 端点

### 1. 构建快照

**POST** `/api/build`

从 Dockerfile 构建快照。

**请求体：**

```json
{
  "dockerfile": "/path/to/Dockerfile",
  "context": "/path/to/context/dir"
}
```

**成功响应：**

```json
{
  "success": true,
  "data": {
    "snapshot_path": "/root/.moulin/snapshots/uuid-dir",
    "snapshot_id": "uuid-string"
  },
  "error": null
}
```

**说明：** `/api/build` 当前返回的是目录型快照路径，而不是 `.tar.gz` 文件。

---

### 2. 启动沙箱

**POST** `/api/start`

从快照启动一个后台运行的沙箱容器。

**请求体：**

```json
{
  "snapshot": "/path/to/snapshot-dir",
  "resources": {
    "memory_bytes": 536870912,
    "cpu_quota": 200000,
    "cpu_period": 100000,
    "pids_max": 128
  }
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `snapshot` | string | 是 | 快照文件路径 |
| `resources` | object | 否 | 资源限制配置（缺省使用默认值） |
| `resources.memory_bytes` | number | 否 | 内存限制（字节），默认 1 GiB |
| `resources.cpu_quota` | number | 否 | CPU 配额（微秒），默认 100000（1 核） |
| `resources.cpu_period` | number | 否 | CPU 周期（微秒），默认 100000 |
| `resources.pids_max` | number | 否 | 最大进程数，默认 512 |

**资源限制示例：**

| 配置 | memory_bytes | cpu_quota | cpu_period | 效果 |
|------|-------------|-----------|------------|------|
| 默认 | 1073741824 | 100000 | 100000 | 1 GiB 内存，1 核 CPU |
| 大内存 | 1073741824 | 100000 | 100000 | 1 GiB 内存，1 核 CPU |
| 2 核 CPU | 1073741824 | 200000 | 100000 | 1 GiB 内存，2 核 CPU |
| 半核 CPU | 1073741824 | 50000 | 100000 | 1 GiB 内存，0.5 核 CPU |

**成功响应：**

```json
{
  "success": true,
  "data": {
    "sandbox_id": "uuid-string"
  },
  "error": null
}
```

**说明：** API 模式下沙箱以 `tail -f /dev/null` 作为守护进程运行，保持沙箱存活，通过 exec API 执行命令。该沙箱使用 `SandboxProfile::Runtime`，会启用更严格的 seccomp、`PR_SET_NO_NEW_PRIVS`、capability 清理，以及额外的 `/proc`/`/sys` 保护。

---

### 3. 创建快照

**POST** `/api/snapshot`

将运行中的沙箱打包为新的快照。

**请求体：**

```json
{
  "sandbox_id": "uuid-string",
  "output": "/path/to/output.tar.gz"
}
```

**成功响应：**

```json
{
  "success": true,
  "data": "/path/to/output.tar.gz",
  "error": null
}
```

---

### 4. 列出资源

**GET** `/api/list`

列出所有 snapshot 和 sandbox。

**成功响应：**

```json
{
  "success": true,
  "data": {
    "snapshots": [
      {
        "id": "uuid",
        "path": "/root/.moulin/snapshots/uuid-dir",
        "created_at": "2026-03-03T10:00:00+00:00",
        "entrypoint": null,
        "cmd": null,
        "env": null
      }
    ],
    "sandboxes": [
      {
        "id": "uuid",
        "snapshot_id": "",
        "created_at": "2026-03-03T10:00:00+00:00",
        "dir": "/root/.moulin/sandboxes/uuid"
      }
    ]
  },
  "error": null
}
```

---

### 5. 销毁沙箱

**DELETE** `/api/sandbox/{id}`

销毁指定沙箱，卸载 OverlayFS 并清理相关目录。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**成功响应：**

```json
{
  "success": true,
  "data": "Destroyed sandbox uuid-string",
  "error": null
}
```

---

### 6. 执行命令

**POST** `/api/sandbox/{id}/exec`

在指定沙箱中执行命令。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**请求体：**

```json
{
  "cmd": ["ls", "-la", "/"]
}
```

**成功响应：**

```json
{
  "success": true,
  "data": {
    "stdout": "total 64\ndrwxr-xr-x  ...",
    "stderr": "",
    "exit_code": 0
  },
  "error": null
}
```

**实现方式：** 通过 `nsenter -a -t <sandbox-pid>` 进入目标沙箱已有的 namespace 执行命令，而不是重新 `chroot` 到 merged 目录。

---

### 7. 读取文件

**GET** `/api/sandbox/{id}/file?path=/etc/hostname`

读取沙箱中指定路径的文件内容。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `path` | string | 沙箱内文件路径 |

**成功响应：**

```json
{
  "success": true,
  "data": "moulin\n",
  "error": null
}
```

**安全措施：** 验证实际路径在 merged_dir 内，防止路径穿越攻击。

---

### 8. 写入文件

**POST** `/api/sandbox/{id}/file`

向沙箱写入文件。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**请求体：**

```json
{
  "path": "/app/config.json",
  "content": "{\"key\": \"value\"}"
}
```

**成功响应：**

```json
{
  "success": true,
  "data": "File /app/config.json written successfully",
  "error": null
}
```

**说明：** 自动创建父目录。

---

### 9. 删除文件

**DELETE** `/api/sandbox/{id}/file`

删除沙箱中的文件或目录。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**请求体：**

```json
{
  "path": "/tmp/logs"
}
```

**成功响应：**

```json
{
  "success": true,
  "data": "File /tmp/logs deleted successfully",
  "error": null
}
```

**说明：** 目录使用 `remove_dir_all` 递归删除，文件使用 `remove_file` 删除。

---

### 10. 上传二进制文件

**POST** `/api/sandbox/{id}/upload`

上传二进制文件到沙箱中（使用 Base64 编码传输）。适用于上传 Excel、图片、压缩包等非文本文件。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**请求体：**

```json
{
  "path": "/home/moulin/workspace/data.xlsx",
  "data": "UEsDBBQAAAA..."
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `path` | string | 沙箱内目标文件路径 |
| `data` | string | Base64 编码的文件内容 |

**成功响应：**

```json
{
  "success": true,
  "data": "File /home/moulin/workspace/data.xlsx uploaded successfully (10535055 bytes)",
  "error": null
}
```

**说明：** 服务器支持最大 50 MiB 的请求体。自动创建父目录。

---

### 11. 下载二进制文件

**GET** `/api/sandbox/{id}/download?path=/home/moulin/workspace/data.xlsx`

从沙箱下载文件（以 Base64 编码返回）。适用于下载二进制文件。

**路径参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | string | 沙箱 UUID |

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `path` | string | 沙箱内文件路径 |

**成功响应：**

```json
{
  "success": true,
  "data": "UEsDBBQAAAA...",
  "error": null
}
```

**说明：** 返回的 `data` 字段为 Base64 编码的文件内容，客户端需解码后使用。

---

## 使用示例

```bash
# 启动 API Server
sudo moulin server

# 构建镜像
curl -X POST http://localhost:3000/api/build \
  -H "Content-Type: application/json" \
  -d '{"dockerfile": "/tmp/Dockerfile", "context": "/tmp/"}'

# 启动沙箱
curl -X POST http://localhost:3000/api/start \
  -H "Content-Type: application/json" \
  -d '{"snapshot": "/root/.moulin/snapshots/uuid-dir"}'

# 在沙箱中执行命令
curl -X POST http://localhost:3000/api/sandbox/{id}/exec \
  -H "Content-Type: application/json" \
  -d '{"cmd": ["python3", "-c", "print(\"hello\")"]}'

# 读取文件
curl "http://localhost:3000/api/sandbox/{id}/file?path=/etc/os-release"

# 写入文件
curl -X POST http://localhost:3000/api/sandbox/{id}/file \
  -H "Content-Type: application/json" \
  -d '{"path": "/app/test.py", "content": "print(\"hello\")"}'

# 列出资源
curl http://localhost:3000/api/list

# 销毁沙箱
curl -X DELETE http://localhost:3000/api/sandbox/{id}
```

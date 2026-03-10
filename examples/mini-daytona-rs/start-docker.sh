#!/usr/bin/env bash
# ============================================================
# Mini-Daytona Server — Docker 一键启动脚本
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# ---- 可配置参数 (环境变量覆盖) ----
IMAGE_NAME="${IMAGE_NAME:-mini-daytona-rs}"
CONTAINER_NAME="${CONTAINER_NAME:-mini-daytona-server}"
VOLUME_NAME="${VOLUME_NAME:-daytona-data}"
PORT="${PORT:-3000}"
REBUILD="${REBUILD:-0}"          # 设为 1 强制重新构建
NO_CACHE="${NO_CACHE:-0}"        # 设为 1 构建时不使用缓存

# ---- 检测平台 (Mac M芯片 = arm64) ----
HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"
if [ "$HOST_ARCH" = "arm64" ] || [ "$HOST_ARCH" = "aarch64" ]; then
    DOCKER_PLATFORM="linux/arm64"
else
    DOCKER_PLATFORM="linux/amd64"
fi

# ---- 颜色 ----
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()   { echo -e "${RED}[ERROR]${NC} $*"; }

# ---- 检查 Docker ----
if ! command -v docker &>/dev/null; then
    err "未找到 docker 命令，请先安装 Docker。"
    exit 1
fi

# ---- 停止旧容器 ----
if docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
    warn "已存在容器 ${CONTAINER_NAME}，正在停止并移除..."
    docker rm -f "$CONTAINER_NAME" >/dev/null
fi

# ---- 构建镜像 ----
need_build=0
if [ "$REBUILD" = "1" ]; then
    need_build=1
elif ! docker images -q "$IMAGE_NAME" 2>/dev/null | grep -q .; then
    need_build=1
fi

if [ "$need_build" = "1" ]; then
    info "构建 Docker 镜像 $IMAGE_NAME (平台: $DOCKER_PLATFORM)..."
    build_args="--platform $DOCKER_PLATFORM"
    [ "$NO_CACHE" = "1" ] && build_args="$build_args --no-cache"
    docker build $build_args -t "$IMAGE_NAME" -f Dockerfile .
    ok "镜像构建完成"
else
    info "使用已有镜像 $IMAGE_NAME (设置 REBUILD=1 强制重新构建)"
fi

# ---- 创建数据卷 ----
if ! docker volume ls -q | grep -qx "$VOLUME_NAME"; then
    docker volume create "$VOLUME_NAME" >/dev/null
    info "已创建数据卷 $VOLUME_NAME"
fi

# ---- 启动容器 ----
info "启动容器 $CONTAINER_NAME (端口 $PORT, 平台 $DOCKER_PLATFORM)..."
docker run -d \
    --name "$CONTAINER_NAME" \
    --platform "$DOCKER_PLATFORM" \
    --privileged \
    -p "${PORT}:3000" \
    -e HOME=/var/run/daytona_home \
    -e TMPDIR=/var/run/daytona_home/tmp \
    -v "${VOLUME_NAME}:/var/run/daytona_home" \
    "$IMAGE_NAME" \
    sh -c 'mkdir -p /var/run/daytona_home/tmp && ./target/release/mini-daytona-rs server' \
    >/dev/null

# ---- 等待服务就绪 ----
info "等待服务启动..."
for i in $(seq 1 15); do
    if curl -sf "http://localhost:${PORT}/api/health" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

if curl -sf "http://localhost:${PORT}/api/health" >/dev/null 2>&1; then
    ok "服务已就绪！"
else
    warn "服务可能尚未完全启动，请稍后手动检查"
fi

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "  Mini-Daytona Server 已启动"
echo -e "  平台:      ${CYAN}${DOCKER_PLATFORM}${NC}"
echo -e "  API 地址:  ${CYAN}http://localhost:${PORT}/api${NC}"
echo -e "  容器名称:  ${CONTAINER_NAME}"
echo -e "  数据卷:    ${VOLUME_NAME}"
echo -e ""
echo -e "  查看日志:  docker logs -f ${CONTAINER_NAME}"
echo -e "  停止服务:  docker rm -f ${CONTAINER_NAME}"
echo -e "  运行测试:  node test/run_e2e.js --client"
echo -e "${GREEN}========================================${NC}"

#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

OUTPUT_DIR="${OUTPUT_DIR:-$SCRIPT_DIR/dist}"
REBUILD="${REBUILD:-0}"
NO_CACHE="${NO_CACHE:-0}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()   { echo -e "${RED}[ERROR]${NC} $*"; }

if ! command -v docker &>/dev/null; then
    err "未找到 docker 命令，请先安装 Docker。"
    exit 1
fi

HOST_ARCH="$(uname -m)"
if [ "$HOST_ARCH" = "arm64" ] || [ "$HOST_ARCH" = "aarch64" ]; then
    DOCKER_PLATFORM="linux/arm64"
    ARTIFACT_ARCH="linux-arm64"
else
    DOCKER_PLATFORM="linux/amd64"
    ARTIFACT_ARCH="linux-amd64"
fi

TARGET_DIR="$OUTPUT_DIR/$ARTIFACT_ARCH"

if docker buildx version >/dev/null 2>&1; then
    DOCKER_BUILD_CMD=(docker buildx build)
else
    warn "未检测到 docker buildx，回退到 docker build（需要启用 BuildKit 才能导出产物）"
    DOCKER_BUILD_CMD=(docker build)
fi

if [ "$REBUILD" = "1" ] && [ -d "$TARGET_DIR" ]; then
    info "清理旧产物目录: $TARGET_DIR"
    rm -rf "$TARGET_DIR"
fi

mkdir -p "$TARGET_DIR"

build_args=(
    --platform "$DOCKER_PLATFORM"
    --target artifact
    --output "type=local,dest=$TARGET_DIR"
    -f Dockerfile
)

if [ "$NO_CACHE" = "1" ]; then
    build_args+=(--no-cache)
fi

build_args+=(.)

info "导出 Linux 二进制产物到 $TARGET_DIR (平台: $DOCKER_PLATFORM)..."
"${DOCKER_BUILD_CMD[@]}" "${build_args[@]}"

if [ ! -f "$TARGET_DIR/mini-daytona-rs" ]; then
    err "导出完成，但未找到产物: $TARGET_DIR/mini-daytona-rs"
    exit 1
fi

chmod +x "$TARGET_DIR/mini-daytona-rs"
ok "产物导出完成: $TARGET_DIR/mini-daytona-rs"
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "  导出产物:  ${CYAN}$TARGET_DIR/mini-daytona-rs${NC}"
echo -e "  目标平台:  ${CYAN}$DOCKER_PLATFORM${NC}"
echo -e ""
echo -e "  Linux 运行: BINARY_PATH=$TARGET_DIR/mini-daytona-rs SKIP_BUILD=1 bash start-local.sh"
echo -e "${GREEN}========================================${NC}"
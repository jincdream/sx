#!/usr/bin/env bash
[ -n "${BASH_VERSION:-}" ] || exec bash "$0" "$@"
# ============================================================
# moulin Server — 本地一键启动脚本
# ============================================================
#
# Linux:
#   - 使用完整隔离模式
#   - 需要 Rust 工具链和系统依赖
#   - 需要 root 权限 (namespace / cgroup / overlay)
#
# macOS:
#   - 使用降级模式
#   - 无 namespace/cgroup/seccomp 隔离，仅用于开发/测试
#   - 无需 root
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# ---- 可配置参数 ----
PORT="${PORT:-3000}"
BUILD_MODE="${BUILD_MODE:-release}"   # release 或 debug
DATA_DIR="${DATA_DIR:-$HOME/.moulin}"
SKIP_BUILD="${SKIP_BUILD:-0}"         # 设为 1 跳过编译
BINARY_PATH="${BINARY_PATH:-}"
HOST_OS="$(uname -s)"

# ---- 颜色 ----
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()   { echo -e "${RED}[ERROR]${NC} $*"; }

# ---- 依赖检查 ----
missing=()

USING_PREBUILT_BINARY=0
if [ -n "$BINARY_PATH" ]; then
    USING_PREBUILT_BINARY=1
fi

if [ "$USING_PREBUILT_BINARY" != "1" ]; then
    command -v cargo &>/dev/null || missing+=("cargo (Rust 工具链)")
fi

if [ "$HOST_OS" = "Linux" ]; then
    command -v ip &>/dev/null || missing+=("iproute2")
    command -v iptables &>/dev/null || missing+=("iptables")

    if [ "$USING_PREBUILT_BINARY" != "1" ]; then
        command -v pkg-config &>/dev/null || missing+=("pkg-config")

        if ! pkg-config --exists libseccomp 2>/dev/null; then
            missing+=("libseccomp-dev")
        fi
        if ! pkg-config --exists openssl 2>/dev/null; then
            missing+=("libssl-dev")
        fi
    fi
elif [ "$HOST_OS" = "Darwin" ]; then
    if [ "$USING_PREBUILT_BINARY" != "1" ]; then
        command -v clang &>/dev/null || missing+=("clang (Xcode Command Line Tools)")
        command -v pkg-config &>/dev/null || missing+=("pkg-config")

        if command -v pkg-config &>/dev/null && ! pkg-config --exists openssl 2>/dev/null; then
            missing+=("openssl")
        fi
    fi
else
    err "当前系统暂不支持: $HOST_OS"
    err "仅支持 Linux 和 macOS"
    exit 1
fi

if [ ${#missing[@]} -gt 0 ]; then
    err "缺少以下依赖:"
    for dep in "${missing[@]}"; do
        echo "  - $dep"
    done
    echo ""
    if [ "$HOST_OS" = "Linux" ]; then
        info "Ubuntu/Debian 安装命令:"
        echo "  sudo apt-get install -y pkg-config libssl-dev libseccomp-dev iproute2 iptables"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    else
        info "macOS 安装命令:"
        echo "  xcode-select --install"
        echo "  brew install pkg-config openssl"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    fi
    exit 1
fi

# ---- Linux 权限检查 ----
if [ "$HOST_OS" = "Linux" ] && [ "$(id -u)" -ne 0 ]; then
    warn "Linux 完整隔离模式需要 root 权限运行"
    info "正在使用 sudo 重新启动..."
    exec sudo -E PORT="$PORT" BUILD_MODE="$BUILD_MODE" DATA_DIR="$DATA_DIR" SKIP_BUILD="$SKIP_BUILD" bash "$0" "$@"
fi

# ---- 编译 ----
if [ "$USING_PREBUILT_BINARY" = "1" ]; then
    info "使用现成二进制: $BINARY_PATH"
elif [ "$SKIP_BUILD" != "1" ]; then
    info "编译项目 (${BUILD_MODE} 模式)..."
    if [ "$BUILD_MODE" = "release" ]; then
        cargo build --release
    else
        cargo build
    fi
    ok "编译完成"
else
    info "跳过编译 (SKIP_BUILD=1)"
fi

# ---- 确定二进制路径 ----
if [ "$USING_PREBUILT_BINARY" = "1" ]; then
    BINARY="$BINARY_PATH"
elif [ "$BUILD_MODE" = "release" ]; then
    BINARY="./target/release/moulin"
else
    BINARY="./target/debug/moulin"
fi

if [ ! -f "$BINARY" ]; then
    err "找不到二进制文件: $BINARY"
    if [ "$USING_PREBUILT_BINARY" = "1" ]; then
        err "请确认 BINARY_PATH 指向可执行文件"
    else
        err "请先编译: cargo build --${BUILD_MODE}"
    fi
    exit 1
fi

# ---- 准备数据目录 ----
mkdir -p "$DATA_DIR/tmp"

# ---- 启动服务 ----
export HOME="$DATA_DIR"
export TMPDIR="$DATA_DIR/tmp"
export MINI_MOULIN_PORT="$PORT"

MODE_LABEL="Linux 完整隔离模式"
if [ "$HOST_OS" = "Darwin" ]; then
    MODE_LABEL="macOS 降级模式 (无隔离)"
    info "检测到 macOS 系统 — 使用降级模式 (无沙箱隔离, 仅用于开发/测试)"
    warn "macOS 模式下没有 namespace/cgroup/seccomp, 安全性降低"
fi

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "  moulin Server 本地启动"
echo -e "  模式:      ${MODE_LABEL}"
echo -e "  API 地址:  ${CYAN}http://localhost:${PORT}/api${NC}"
echo -e "  数据目录:  ${DATA_DIR}"
echo -e "  二进制:    ${BINARY}"
echo -e ""
echo -e "  停止服务:  Ctrl+C"
echo -e "  运行测试:  node test/run_e2e.js --client"
echo -e "${GREEN}========================================${NC}"
echo ""

info "启动服务器..."
exec "$BINARY" server

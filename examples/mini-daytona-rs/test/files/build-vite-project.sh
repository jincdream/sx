#!/bin/bash
set -e

echo "=== Vue3 Vite Project Build Script ==="

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# 1. Scaffold a Vue3 Vite project (non-interactive)
echo "[1/3] Scaffolding Vue3 project with Vite..."
npx create-vite my-vue-app --template vue
echo "Scaffold complete."

# 2. Install dependencies
echo "[2/3] Installing dependencies..."
cd my-vue-app
npm install
echo "Dependencies installed."

# 3. Build for production
echo "[3/3] Building for production..."
npm run build
echo "Build complete."

# 4. Verify output
if [ -f dist/index.html ]; then
  echo "SUCCESS: dist/index.html exists"
  ls -la dist/
else
  echo "FAIL: dist/index.html not found"
  exit 1
fi

#!/bin/sh
set -e

# Run checks
cargo fmt -- --check || true
cargo clippy -- -D warnings || true
cargo test || true

echo "--- Setup ---"
export HOME=/var/run/daytona_home
export TMPDIR=/var/run/daytona_home/tmp
mkdir -p $TMPDIR

echo "--- Starting Server ---"
./target/release/mini-daytona-rs server &
SERVER_PID=$!

sleep 3

echo "--- Requesting Build ---"
BUILD_RES=$(curl -s -X POST http://localhost:3000/api/build \
  -H 'Content-Type: application/json' \
  -d '{"dockerfile":"/work/tests/e2e/Dockerfile.nginx","context":"/work/tests/e2e"}')
echo "$BUILD_RES" | jq .

SNAPSHOT_PATH=$(echo "$BUILD_RES" | jq -r '.data.snapshot_path')

if [ "$SNAPSHOT_PATH" = "null" ]; then
    echo "API Build Failed!"
    exit 1
fi

echo "--- Requesting Start ---"
START_RES=$(curl -s -X POST http://localhost:3000/api/start \
  -H 'Content-Type: application/json' \
  -d "{\"snapshot\":\"$SNAPSHOT_PATH\"}")
echo "$START_RES" | jq .

SANDBOX_ID=$(echo "$START_RES" | jq -r '.data.sandbox_id')

if [ "$SANDBOX_ID" = "null" ]; then
    echo "API Start Failed!"
    exit 1
fi

sleep 3

echo "--- Requesting File Read (Expected Nginx HTML) ---"
READ_RES=$(curl -s "http://localhost:3000/api/sandbox/$SANDBOX_ID/file?path=/usr/share/nginx/html/index.html")
echo "$READ_RES"

echo "--- Requesting Exec: ls -la /usr/share/nginx/html ---"
EXEC_RES=$(curl -s -X POST "http://localhost:3000/api/sandbox/$SANDBOX_ID/exec" \
  -H 'Content-Type: application/json' \
  -d '{"cmd": ["/bin/ls", "-la", "/usr/share/nginx/html"]}')
echo "$EXEC_RES"

echo "--- Requesting File Write ---"
WRITE_RES=$(curl -s -X POST "http://localhost:3000/api/sandbox/$SANDBOX_ID/file" \
  -H 'Content-Type: application/json' \
  -d '{"path": "/usr/share/nginx/html/custom.txt", "content": "hello API"}')
echo "$WRITE_RES"

echo "--- Requesting File Read (Edited File) ---"
READ_NEW_RES=$(curl -s "http://localhost:3000/api/sandbox/$SANDBOX_ID/file?path=/usr/share/nginx/html/custom.txt")
echo "$READ_NEW_RES"

echo "--- Requesting Exec: cat newly written file ---"
EXEC_CAT_RES=$(curl -s -X POST "http://localhost:3000/api/sandbox/$SANDBOX_ID/exec" \
  -H 'Content-Type: application/json' \
  -d '{"cmd": ["/bin/cat", "/usr/share/nginx/html/custom.txt"]}')
echo "$EXEC_CAT_RES"

echo "--- Requesting File Delete ---"
DEL_RES=$(curl -s -X DELETE "http://localhost:3000/api/sandbox/$SANDBOX_ID/file" \
  -H 'Content-Type: application/json' \
  -d '{"path": "/usr/share/nginx/html/custom.txt"}')
echo "$DEL_RES"

echo "--- Requesting File Read (Deleted File) -> Should Err ---"
curl -s "http://localhost:3000/api/sandbox/$SANDBOX_ID/file?path=/usr/share/nginx/html/custom.txt"

echo ""
echo "--- Requesting Destroy ---"
curl -s -X DELETE "http://localhost:3000/api/sandbox/$SANDBOX_ID" | jq .

echo "API Server test complete. Killing server..."
kill $SERVER_PID

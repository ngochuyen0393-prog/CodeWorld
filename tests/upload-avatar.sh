#!/usr/bin/env bash
# D-017 收图通道冒烟脚本（人工验收用）：上传 fixtures 图 -> 断言 ok:true + 落盘
#
# 前置：世界已启动（start.bat 或 cargo run -p codeworld），WS 端口默认 8080。
# 用法：bash tests/upload-avatar.sh [端口]
#
# 断言：
#   1. POST /upload/avatar 返回 {"ok":true,"asset_id":"av_..."}
#   2. assets/avatars/{id}.png 与 {id}.json 落盘
#   3. GET /assets/{id}.png 原图可直取（字节回读一致）
# 注：重启后仍在（启动扫目录重建）由 Rust 测试 registry_load_rebuilds_and_skips_corrupt
#     覆盖；浏览器调试面板肉眼验收见任务卡报告"人工验收清单"。
set -euo pipefail

PORT="${1:-8080}"
BASE="http://127.0.0.1:$PORT"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="$ROOT/tests/fixtures/test-avatar.png"

if [ ! -f "$FIXTURE" ]; then
  echo "FAIL: fixture missing: $FIXTURE"
  echo "  generate it first: cargo test -p codeworld --release -- --ignored generate_avatar_fixture"
  exit 1
fi

echo "[1/3] POST $BASE/upload/avatar ..."
RESP="$(curl -s -f \
  -F "file=@$FIXTURE;type=image/png" \
  -F "name=avatar-smoke" \
  -F "height_world=2.0" \
  "$BASE/upload/avatar")"
echo "  resp: $RESP"

case "$RESP" in
  *'"ok":true'*) : ;;
  *) echo "FAIL: expect ok:true"; exit 1 ;;
esac

ASSET_ID="$(printf '%s' "$RESP" | sed -n 's/.*"asset_id":"\([A-Za-z0-9_]*\)".*/\1/p')"
if [ -z "$ASSET_ID" ]; then
  echo "FAIL: asset_id not found in response"
  exit 1
fi
echo "  asset_id: $ASSET_ID"

echo "[2/3] check files on disk ..."
PNG_PATH="$ROOT/assets/avatars/$ASSET_ID.png"
META_PATH="$ROOT/assets/avatars/$ASSET_ID.json"
[ -f "$PNG_PATH" ] || { echo "FAIL: $PNG_PATH missing"; exit 1; }
[ -f "$META_PATH" ] || { echo "FAIL: $META_PATH missing"; exit 1; }
echo "  png:  $PNG_PATH ($(wc -c < "$PNG_PATH") bytes)"
echo "  meta: $META_PATH"

echo "[3/3] GET $BASE/assets/$ASSET_ID.png ..."
curl -s -f "$BASE/assets/$ASSET_ID.png" -o /tmp/cw-avatar-roundtrip.png
if ! cmp -s "$PNG_PATH" /tmp/cw-avatar-roundtrip.png; then
  echo "FAIL: roundtrip bytes differ"
  exit 1
fi
rm -f /tmp/cw-avatar-roundtrip.png

echo "PASS: upload -> disk -> serve roundtrip OK (asset_id=$ASSET_ID)"

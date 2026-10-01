#!/usr/bin/env bash
#
# 打包：产出 docs/03-FFI-SIGNING-PACKAGING.md §5 约定的产物目录结构。
#
#   ./script/package.sh                 # 当前平台，debug 版本号来自 Cargo.toml
#   PROFILE=release ./script/package.sh # 发布构建（默认就是 release）
#   XSPIDER_ARIA2_PATH=/path/to/aria2next ./script/package.sh   # 顺带把 Aria2Next 打进包
#
# 产物结构（docs/03 §5）：
#
#   dist/xspiderd-<ver>-<platform>-<arch>.tar.gz
#   ├── xspiderd                 # sidecar 可执行文件（macOS 上 ad-hoc 签名）
#   ├── aria2next                # 可选：A 方案随包携带（附 NOTICE 与版本说明）
#   ├── libxspider.dylib|.so|.dll# 次形态：cdylib
#   ├── xspider.schema.json      # 机器可读契约
#   ├── LICENSE  NOTICE          # GPL-3.0-only + Aria2Next 的 GPL-2.0 声明与源码链接
#   └── CHANGELOG.md
#
# 为什么要脚本而不是手工敲：**签名与产物名是最容易漏的两件事**。
# macOS arm64 上未签名的可执行文件会被内核 SIGKILL（退出码 137，静默死亡，
# 见 docs/03 §1），而 cdylib 的产品名必须与文档一致（ADR-012）。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
[ -x "$CARGO" ] || CARGO="cargo"

PROFILE="${PROFILE:-release}"
if [ "$PROFILE" = "release" ]; then
  TARGET_DIR="release"
else
  TARGET_DIR="debug"
fi

step() { printf '\n=== %s ===\n' "$1"; }
ok()   { printf '[ok] %s\n' "$1"; }
fail() { printf '[!!] %s\n' "$1" >&2; exit 1; }

# ---------------------------------------------------------------- 版本与平台
VERSION="$(python3 - <<'PY'
import re, pathlib
text = pathlib.Path("Cargo.toml").read_text()
m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
print(m.group(1) if m else "0.0.0")
PY
)"
[ -n "$VERSION" ] || fail "读不到 workspace 版本号"

case "$(uname -s)" in
  Darwin) PLATFORM="macos";   CDYLIB="libxspider.dylib" ;;
  Linux)  PLATFORM="linux";   CDYLIB="libxspider.so" ;;
  MINGW*|MSYS*|CYGWIN*) PLATFORM="windows"; CDYLIB="xspider.dll" ;;
  *) fail "不认识的平台：$(uname -s)" ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *) ARCH="$(uname -m)" ;;
esac

NAME="xspiderd-${VERSION}-${PLATFORM}-${ARCH}"
OUT="$ROOT/dist/$NAME"
rm -rf "$OUT"
mkdir -p "$OUT"

step "1/4 构建（${PROFILE}/${PLATFORM}-${ARCH}）"
if [ "$PROFILE" = "release" ]; then
  "$CARGO" build --release -p xspiderd -p xspider-ffi >/dev/null
else
  "$CARGO" build -p xspiderd -p xspider-ffi >/dev/null
fi
ok "构建完成"

BIN="target/$TARGET_DIR/xspiderd"
LIB="target/$TARGET_DIR/$CDYLIB"
[ -f "$BIN" ] || fail "找不到 $BIN"
[ -f "$LIB" ] || fail "找不到 $LIB（cdylib 的产品名必须是 libxspider.*，见 ADR-012）"

step "2/4 组装产物"
cp "$BIN" "$OUT/"
cp "$LIB" "$OUT/"
cp contract/xspider.schema.json "$OUT/"
cp LICENSE NOTICE "$OUT/"
[ -f CHANGELOG.md ] && cp CHANGELOG.md "$OUT/"
ok "已放入 xspiderd / $CDYLIB / schema / LICENSE / NOTICE"

# Aria2Next 是**可选**携带项（docs/03 §4 的 A 方案）。带上它就必须附版本说明与来源。
if [ -n "${XSPIDER_ARIA2_PATH:-}" ] && [ -f "${XSPIDER_ARIA2_PATH}" ]; then
  ARIA2_VERSION="$("$XSPIDER_ARIA2_PATH" --version 2>/dev/null | head -1 || echo '未知版本')"
  cp "$XSPIDER_ARIA2_PATH" "$OUT/aria2next"
  {
    echo "本包内含的 Aria2Next"
    echo "  版本：${ARIA2_VERSION}"
    echo '  许可证：GPL-2.0（version 2, or (at your option) any later version）' 
    echo "  来源与源码：https://github.com/AnInsomniacy/aria2-next"
    echo "  校验和（sha256）："
    ( cd "$OUT" && shasum -a 256 aria2next 2>/dev/null || sha256sum aria2next )
    echo
    echo "GPL-2.0 要求随二进制分发时提供许可证文本与源码获取方式；"
    echo "完整的第三方声明见同目录的 NOTICE，源码见上方链接。"
  } > "$OUT/ARIA2NEXT-NOTICE.txt"
  ok "已携带 Aria2Next（${ARIA2_VERSION}）并附 ARIA2NEXT-NOTICE.txt"
else
  ok "未携带 Aria2Next（设 XSPIDER_ARIA2_PATH 可携带；外壳也可自行提供路径）"
fi

step "3/4 签名（macOS 必需）"
if [ "$PLATFORM" = "macos" ]; then
  # arm64 要求每个可执行代码文件自己有有效签名；ad-hoc 即可（docs/03 §1）
  for file in xspiderd aria2next; do
    [ -f "$OUT/$file" ] && codesign -f -s - "$OUT/$file" >/dev/null 2>&1 && ok "已 ad-hoc 签名 $file"
  done
  codesign -f -s - "$OUT/$CDYLIB" >/dev/null 2>&1 && ok "已 ad-hoc 签名 $CDYLIB"
  # 自检：签名必须真的有效，否则装到别的机器上会静默 137
  codesign --verify --strict "$OUT/xspiderd" >/dev/null 2>&1 || fail "xspiderd 签名校验失败"
  codesign --verify --strict "$OUT/$CDYLIB" >/dev/null 2>&1 || fail "$CDYLIB 签名校验失败"
  ok "签名校验通过"
else
  ok "非 macOS，跳过签名（Windows 需要 Authenticode 才不弹 SmartScreen）"
fi

step "4/4 打包"
( cd "$ROOT/dist" && tar -czf "${NAME}.tar.gz" "$NAME" )
( cd "$ROOT/dist" && { shasum -a 256 "${NAME}.tar.gz" 2>/dev/null || sha256sum "${NAME}.tar.gz"; } > "${NAME}.tar.gz.sha256" )
ok "dist/${NAME}.tar.gz"
cat "dist/${NAME}.tar.gz.sha256"

printf '\n产物目录：\n'
( cd "$OUT" && ls -la | sed 's/^/  /' )
printf '\n自检：\n'
"$OUT/xspiderd" --version | sed 's/^/  /'

printf '\n打包完成。cdylib 可用 script/cdylib_check.c 验证：\n'
printf '  cc -o /tmp/cdylib_check script/cdylib_check.c && /tmp/cdylib_check %s/%s\n' "$OUT" "$CDYLIB"

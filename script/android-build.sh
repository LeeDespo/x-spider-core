#!/usr/bin/env bash
# Build and package Android sidecar + cdylib with NDK clang (no cargo-ndk).
# Usage: script/android-build.sh [arm64-v8a] [x86_64]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

step() { printf '\n=== %s ===\n' "$1"; }
fail() { printf '[!!] %s\n' "$1" >&2; exit 1; }
ok() { printf '[ok] %s\n' "$1"; }

usage() {
  cat <<'EOF'
用法：script/android-build.sh [arm64-v8a] [x86_64]

使用 NDK 27.3.13750724 与 rustup Cargo 构建并打包 Android native libraries。
默认只构建 arm64-v8a；可同时传入 arm64-v8a 与 x86_64。

环境变量：
  ANDROID_NDK_HOME       NDK 27.3.13750724 路径（未设置时探测常见 SDK 目录）
  ANDROID_ABIS           未传 ABI 参数时的空格分隔 ABI 列表
  PROFILE                release（默认）或 debug
  ANDROID_CARGO_OFFLINE  1 默认离线；设为 0 时允许 Cargo 下载依赖

首次构建若提示缺少 crate，先运行 `~/.cargo/bin/cargo fetch --locked`，再重试。
包内 sidecar 是 PIE 可执行文件，名为 jniLibs/<abi>/libxspiderd.so；输出不是 APK。
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac

# Homebrew cargo is a standalone binary and ignores rust-toolchain.toml. Put
# rustup's shims first and require that cargo resolves from this directory.
RUSTUP_BIN="$HOME/.cargo/bin"
[ -x "$RUSTUP_BIN/cargo" ] || fail "找不到 rustup cargo：$RUSTUP_BIN/cargo"
PATH="$RUSTUP_BIN:$PATH"
export PATH
CARGO="$(command -v cargo)"
case "$CARGO" in
  "$RUSTUP_BIN"/*) ;;
  *) fail "cargo 没有解析到 rustup shim：$CARGO" ;;
esac
ok "使用 rustup cargo：$CARGO ($($CARGO --version))"

NDK_VERSION="27.3.13750724"
NDK=""
for candidate in \
  "${ANDROID_NDK_HOME:-}" \
  "${ANDROID_NDK_ROOT:-}" \
  "${ANDROID_SDK_ROOT:-}/ndk/${NDK_VERSION}" \
  "${ANDROID_HOME:-}/ndk/${NDK_VERSION}" \
  "$HOME/Library/Android/sdk/ndk/${NDK_VERSION}" \
  "$HOME/Android/Sdk/ndk/${NDK_VERSION}"; do
  if [ -n "$candidate" ] && [ -x "$candidate/ndk-build" ]; then
    NDK="$candidate"
    break
  fi
done
[ -n "$NDK" ] || fail "需要 Android NDK ${NDK_VERSION}；设置 ANDROID_NDK_HOME 指向该版本目录"
NDK_ACTUAL="$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -1)"
[ "$NDK_ACTUAL" = "$NDK_VERSION" ] || fail "NDK 版本不符：需要 ${NDK_VERSION}，实际为 ${NDK_ACTUAL:-未知}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    if [ -x "$NDK/toolchains/llvm/prebuilt/darwin-arm64/bin/llvm-readelf" ]; then
      NDK_HOST="darwin-arm64"
    else
      # Some NDK releases keep the universal clang bundle under this name.
      NDK_HOST="darwin-x86_64"
    fi
    ;;
  Darwin-x86_64) NDK_HOST="darwin-x86_64" ;;
  Linux-x86_64) NDK_HOST="linux-x86_64" ;;
  *) fail "NDK host 暂不支持：$(uname -s)-$(uname -m)" ;;
esac
TOOLBIN="$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin"
[ -x "$TOOLBIN/llvm-readelf" ] || fail "NDK 缺少 llvm-readelf：$TOOLBIN"
READELF="$TOOLBIN/llvm-readelf"

PROFILE="${PROFILE:-release}"
case "$PROFILE" in
  release) BUILD_ARGS="--release"; TARGET_DIR="release" ;;
  debug) BUILD_ARGS=""; TARGET_DIR="debug" ;;
  *) fail "PROFILE 仅支持 release 或 debug" ;;
esac
CALLER_RUSTFLAGS="${RUSTFLAGS:-}"

if [ "$#" -gt 0 ]; then
  ABI_LIST="$*"
else
  ABI_LIST="${ANDROID_ABIS:-arm64-v8a}"
fi
[ -n "$ABI_LIST" ] || fail "至少指定一个 ABI"

VERSION="$(python3 - <<'PY'
import pathlib, re
text = pathlib.Path("Cargo.toml").read_text()
match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
print(match.group(1) if match else "")
PY
)"
[ -n "$VERSION" ] || fail "读不到 workspace 版本号"
[ -f LICENSE.aria2 ] || fail "缺少 LICENSE.aria2（Aria2Next GPL-2.0 完整文本）"

for abi in $ABI_LIST; do
  case "$abi" in
    arm64-v8a)
      target="aarch64-linux-android"
      cargo_key="AARCH64_LINUX_ANDROID"
      cc_target="aarch64-linux-android"
      machine="AArch64"
      ;;
    x86_64)
      target="x86_64-linux-android"
      cargo_key="X86_64_LINUX_ANDROID"
      cc_target="x86_64-linux-android"
      machine="Advanced Micro Devices X86-64"
      ;;
    *) fail "不支持的 ABI：$abi（支持 arm64-v8a 与 x86_64）" ;;
  esac

  linker="$TOOLBIN/${cc_target}23-clang"
  [ -x "$linker" ] || fail "NDK 缺少 API 23 clang：$linker"
  rustup target list --installed | grep -Fxq "$target" || rustup target add "$target"

  # cc-rs accepts the underscored target spelling. Rust code links with the API
  # 23 clang wrapper, and lld emits 16 KiB LOAD alignment.
  export "CARGO_TARGET_${cargo_key}_LINKER=$linker"
  target_flags_var="CARGO_TARGET_${cargo_key}_RUSTFLAGS"
  prior_target_rustflags="${!target_flags_var:-}"
  target_rustflags="${CALLER_RUSTFLAGS} ${prior_target_rustflags} -C link-arg=-Wl,-z,max-page-size=16384"
  export "CARGO_TARGET_${cargo_key}_RUSTFLAGS=$target_rustflags"
  export "CC_${target//-/_}=$linker"
  export "AR_${target//-/_}=$TOOLBIN/llvm-ar"
  # Keep CI's -D warnings (and any caller flags) target-specific so the GNU
  # lld -z option never leaks into host proc-macro builds on macOS.
  unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS

  step "构建 ${abi} / ${target}（NDK ${NDK_VERSION}, API 23, 16 KiB pages）"
  cargo_build() {
    if [ "${ANDROID_CARGO_OFFLINE:-1}" = "1" ]; then
      # shellcheck disable=SC2086
      "$CARGO" build --locked --offline $BUILD_ARGS --target "$target" -p xspiderd -p xspider-ffi
    else
      # shellcheck disable=SC2086
      "$CARGO" build --locked $BUILD_ARGS --target "$target" -p xspiderd -p xspider-ffi
    fi
  }
  cargo_build

  bin="$ROOT/target/$target/$TARGET_DIR/xspiderd"
  lib="$ROOT/target/$target/$TARGET_DIR/libxspider.so"
  [ -f "$bin" ] || fail "找不到目标 sidecar：$bin"
  [ -f "$lib" ] || fail "找不到目标 cdylib：$lib"

  step "验证 ELF 类型、ABI、C ABI 符号与 16 KiB 页对齐"
  python3 - "$READELF" "$bin" "$lib" "$machine" <<'PY'
import re, subprocess, sys

readelf, expected_machine = sys.argv[1], sys.argv[4]
for path in sys.argv[2:4]:
    header = subprocess.check_output([readelf, "-hW", path], text=True)
    if not re.search(r"Type:\s+DYN\b", header):
        raise SystemExit(f"{path}: 不是 Android 可执行 PIE / shared object（ELF type 必须是 DYN）")
    if expected_machine not in header:
        raise SystemExit(f"{path}: ELF machine 与 ABI 不符；预期 {expected_machine}")
    program = subprocess.check_output([readelf, "-lW", path], text=True)
    loads = [line for line in program.splitlines() if re.match(r"\s*LOAD\s", line)]
    if not loads:
        raise SystemExit(f"{path}: ELF 没有 LOAD program header")
    alignments = []
    for line in loads:
        value = line.split()[-1]
        alignment = int(value, 16) if value.startswith("0x") else int(value, 16)
        alignments.append(alignment)
    if min(alignments) < 16384:
        raise SystemExit(f"{path}: LOAD 对齐小于 16 KiB：{[hex(v) for v in alignments]}")
    print(f"[ok] {path}: DYN / {expected_machine} / LOAD align={[hex(v) for v in alignments]}")

dynamic = subprocess.check_output([readelf, "-dW", sys.argv[2]], text=True)
if "PIE" not in dynamic:
    raise SystemExit(f"{sys.argv[2]}: 缺少 PIE 标记")
symbols = subprocess.check_output([readelf, "--dyn-syms", "--wide", sys.argv[3]], text=True)
for symbol in ("xspider_version", "xspider_call", "xspider_free"):
    if not re.search(rf"\b{symbol}$", symbols, re.M):
        raise SystemExit(f"{sys.argv[3]}: 缺少导出符号 {symbol}")
print("[ok] sidecar 标记为 PIE，cdylib 导出三个 C ABI 符号")
PY

  name="xspiderd-${VERSION}-android-${abi}"
  out="$ROOT/dist/$name"
  rm -rf "$out"
  mkdir -p "$out/jniLibs/$abi" "$out/test-fixtures"
  install -m 0755 "$bin" "$out/jniLibs/$abi/libxspiderd.so"
  install -m 0644 "$lib" "$out/jniLibs/$abi/libxspider.so"
  cp contract/xspider.schema.json LICENSE LICENSE.aria2 NOTICE "$out/"
  python3 - "$out/test-fixtures/fixtures" <<'PY'
import pathlib, shutil, subprocess, sys

destination = pathlib.Path(sys.argv[1])
files = subprocess.check_output(["git", "ls-files", "-z", "--", "fixtures"]).split(b"\0")
copied = 0
for raw in files:
    if not raw:
        continue
    relative = pathlib.Path(raw.decode("utf-8"))
    if "raw" in relative.parts or ".tmp" in relative.parts:
        continue
    source = pathlib.Path.cwd() / relative
    if source.is_file():
        target = destination / pathlib.Path(*relative.parts[1:])
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        copied += 1
if not copied:
    raise SystemExit("没有可打包的 Git 跟踪 fixtures；拒绝生成空测试目录")
print(f"[ok] 只复制 {copied} 个 Git 跟踪 fixture 文件；忽略 raw/.tmp")
PY
  cat > "$out/ANDROID-BUILD.txt" <<EOF
X-Spider Core Android native outputs
====================================
ABI: ${abi}
Rust target: ${target}
Android NDK: ${NDK_VERSION}
Minimum Android API: 23
ELF PT_LOAD alignment: at least 16384 bytes (16 KiB)

jniLibs/${abi}/libxspider.so is the C ABI library.
jniLibs/${abi}/libxspiderd.so is an executable PIE packaged under the
native-library name so Android APK tooling extracts it into nativeLibraryDir.
An APK host that starts this sidecar must use nativeLibraryDir and enable
legacy native-library packaging as required by Android's extracted-file rules.
test-fixtures/ is separate from the runtime libraries and is for offline tests.
This directory is a native package, not a complete APK.
EOF
  (cd "$out" && shasum -a 256 "jniLibs/$abi/libxspiderd.so" "jniLibs/$abi/libxspider.so" 2>/dev/null || \
    sha256sum "jniLibs/$abi/libxspiderd.so" "jniLibs/$abi/libxspider.so") > "$out/SHA256SUMS"
  (cd "$ROOT/dist" && tar -czf "${name}.tar.gz" "$name")
  (cd "$ROOT/dist" && { shasum -a 256 "${name}.tar.gz" 2>/dev/null || sha256sum "${name}.tar.gz"; } > "${name}.tar.gz.sha256")
  ok "已打包 dist/${name}.tar.gz"
done

printf '\nAndroid 包目录：\n'
for abi in $ABI_LIST; do
  name="xspiderd-${VERSION}-android-${abi}"
  printf '  dist/%s\n' "$name"
done

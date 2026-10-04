#!/usr/bin/env bash
#
# 冒烟：一条命令验证「双形态 + 垂直切片」
# （构建 → cdylib dlopen → sidecar 握手/调用/关停 → CLI 只经契约跑一遍）。
#
#   ./script/smoke.sh              # 默认离线：用 fixtures 回放，不碰网络
#   XSPIDER_LIVE=1 ./script/smoke.sh   # 打真 X（需要 XSPIDER_COOKIE，可选 XSPIDER_PROXY）
#
# 覆盖：
#   1. 构建（含 cdylib）
#   2. cdylib 形态：dlopen + 调三个 C ABI 函数
#   3. sidecar 形态：--port 0 读 ready 行 → curl 调 system.version 与 fetch.get_user → 断言字段
#   4. 优雅退出 + **无残留进程**（docs/05 §6 明确点名要断言这一条）
#
# 失败即退出码非 0，CI 与本地都直接可用。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# PATH 里可能排着 Homebrew 的 cargo（它不读 rust-toolchain.toml），优先用 rustup 的
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
[ -x "$CARGO" ] || CARGO="cargo"

PROFILE="${PROFILE:-debug}"
if [ "$PROFILE" = "release" ]; then
  TARGET_DIR="release"
else
  TARGET_DIR="debug"
fi

# 注意：macOS 自带 bash 3.2，在 `set -u` 下展开**空数组**会报 unbound variable。
# 所以这里不用数组拼参数，直接用函数分支。
build_pkg() {
  local pkg="$1"
  if [ "$PROFILE" = "release" ]; then
    "$CARGO" build --offline -p "$pkg" --release >/dev/null 2>&1 \
      || "$CARGO" build -p "$pkg" --release >/dev/null
  else
    "$CARGO" build --offline -p "$pkg" >/dev/null 2>&1 \
      || "$CARGO" build -p "$pkg" >/dev/null
  fi
}

TMP_DIR="$(mktemp -d)"
SIDECAR_PID=""
cleanup() {
  if [ -n "$SIDECAR_PID" ] && kill -0 "$SIDECAR_PID" 2>/dev/null; then
    kill -TERM "$SIDECAR_PID" 2>/dev/null || true
    wait "$SIDECAR_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

step() { printf '\n=== %s ===\n' "$1"; }
fail() { printf '[!!] %s\n' "$1" >&2; exit 1; }
ok()   { printf '[ok] %s\n' "$1"; }

# ---------------------------------------------------------------- 1. 构建
step "1/6 构建"
build_pkg xspider-fetch
build_pkg xspider-ffi
build_pkg xspiderd
build_pkg xspider-cli
ok "构建完成（${PROFILE}）"

SIDECAR="target/$TARGET_DIR/xspiderd"
[ -x "$SIDECAR" ] || fail "找不到 $SIDECAR"
CLI="target/$TARGET_DIR/xspider-cli"
[ -x "$CLI" ] || fail "找不到 $CLI"

# ---------------------------------------------------------------- 2. cdylib
step "2/6 cdylib 形态（dlopen + 三个 C ABI 函数）"
case "$(uname -s)" in
  Darwin) CDYLIB="target/$TARGET_DIR/libxspider.dylib" ;;
  Linux)  CDYLIB="target/$TARGET_DIR/libxspider.so" ;;
  *)      CDYLIB="target/$TARGET_DIR/xspider.dll" ;;
esac
[ -f "$CDYLIB" ] || fail "找不到 cdylib：$CDYLIB"

# macOS arm64 要求每个可执行代码文件自己签名（未签名会被内核 SIGKILL，见 docs/03 §1）
if [ "$(uname -s)" = "Darwin" ]; then
  codesign -f -s - "$CDYLIB" >/dev/null 2>&1 || true
fi

CC_BIN="${CC:-cc}"
"$CC_BIN" -o "$TMP_DIR/cdylib_check" script/cdylib_check.c >/dev/null || fail "编译 cdylib_check.c 失败"
"$TMP_DIR/cdylib_check" "$CDYLIB" || fail "cdylib 形态检查失败"
ok "cdylib 形态可用"

# ---------------------------------------------------------------- 3. sidecar
step "3/6 启动 sidecar 并握手（--port 0）"
if [ "${XSPIDER_LIVE:-0}" = "1" ]; then
  [ -n "${XSPIDER_COOKIE:-}" ] || fail "XSPIDER_LIVE=1 需要 XSPIDER_COOKIE"
  MODE_DESC="live（打真 X）"
  TARGET_NAME="${XSPIDER_SMOKE_SCREEN_NAME:-jack}"
else
  MODE_DESC="离线（fixtures 回放）"
  TARGET_NAME="demo_user"
fi
echo "    模式：${MODE_DESC}，目标用户：$TARGET_NAME"

READY_FILE="$TMP_DIR/ready.txt"
stdout_log="$TMP_DIR/sidecar.stdout"
# 本次运行专属的 token：残留检查靠它精确定位"我们启动的那个进程"。
# 用 pgrep -f xspiderd 会把用户在别的终端起的 sidecar 也判成残留（假阳性）。
RUN_TOKEN="smoke-$(date +%s)-$$"
# 日志走 stderr，stdout 只该有 ready 行
if [ "$MODE_DESC" = "live（打真 X）" ]; then
  "$SIDECAR" --port 0 --token "$RUN_TOKEN" >"$stdout_log" 2>"$TMP_DIR/sidecar.stderr" &
else
  "$SIDECAR" --port 0 --token "$RUN_TOKEN" --fixture-dir "$ROOT/fixtures" >"$stdout_log" 2>"$TMP_DIR/sidecar.stderr" &
fi
SIDECAR_PID=$!

for _ in $(seq 1 100); do
  if [ -s "$stdout_log" ] && head -1 "$stdout_log" | grep -q '^ready '; then
    break
  fi
  if ! kill -0 "$SIDECAR_PID" 2>/dev/null; then
    echo "--- sidecar stderr ---" >&2
    tail -40 "$TMP_DIR/sidecar.stderr" >&2 || true
    fail "sidecar 提前退出"
  fi
  sleep 0.1
done
head -1 "$stdout_log" | grep -q '^ready ' || {
  echo "--- sidecar stdout ---" >&2; cat "$stdout_log" >&2
  fail "10s 内没有拿到 ready 行"
}
head -1 "$stdout_log" > "$READY_FILE"

PORT="$(python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).read().strip()[len("ready "):])["port"])' "$READY_FILE")"
TOKEN="$(python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).read().strip()[len("ready "):])["token"])' "$READY_FILE")"
VERSION="$(python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).read().strip()[len("ready "):])["version"])' "$READY_FILE")"
[ -n "$PORT" ] && [ -n "$TOKEN" ] && [ -n "$VERSION" ] || fail "ready 行缺少 port/token/version"
ok "ready {\"port\":$PORT,\"version\":\"$VERSION\",\"token\":<已隐藏，${#TOKEN} 字符>}"
# 单实例纪律：ready 行里绝不该出现 token 之外的东西
[ "$(wc -l < "$stdout_log" | tr -d ' ')" = "1" ] || fail "stdout 里除了 ready 行还有别的东西（日志必须走 stderr）"

rpc() {
  # rpc <method> <params-json> → 打印响应体
  local method="$1" params="$2"
  curl -sS --fail-with-body -X POST "http://127.0.0.1:$PORT/" \
    -H "X-XSpider-Token: $TOKEN" -H 'Content-Type: application/json' \
    --data "$(python3 -c 'import json,sys; print(json.dumps({"method": sys.argv[1], "params": json.loads(sys.argv[2])}))' "$method" "$params")"
}

# ---------------------------------------------------------------- 4. 调用与断言
step "4/6 调 system.version 与 fetch.get_user 并断言"

VERSION_RESP="$(rpc system.version '{}')"
python3 - "$VERSION_RESP" "$VERSION" <<'PY_EOF' || fail "system.version 响应不符合契约"
import json, sys
resp = json.loads(sys.argv[1]); expected = sys.argv[2]
assert "result" in resp, f"缺少 result：{resp}"
assert resp["result"]["contract_version"] == expected, f"契约版本不一致：{resp}"
assert "build_version" in resp["result"], f"缺少 build_version：{resp}"
print(f"[ok] system.version → contract {resp['result']['contract_version']} / build {resp['result']['build_version']}")
PY_EOF

# 凭据从哪里来：live 模式用环境变量里的真 cookie；
# 离线模式用一个固定假 cookie —— fixture 的 match 条件要求"带凭据"，
# 而离线本来就不该有真凭据（凭据只进不出，也不该为了跑测试去要一份真的）。
if [ "${XSPIDER_LIVE:-0}" = "1" ]; then
  COOKIE_JSON="$(python3 -c 'import json,os; print(json.dumps({"cookie": os.environ["XSPIDER_COOKIE"]}))')"
else
  COOKIE_JSON='{"cookie":"auth_token=fixture; ct0=fixture"}'
fi

AUTH_RESP="$(rpc auth.set_cookie "$COOKIE_JSON")"
python3 - "$AUTH_RESP" <<'PY_EOF' || fail "auth.set_cookie 失败"
import json, sys
resp = json.loads(sys.argv[1])
assert resp.get("result", {}).get("ok") is True, f"注入凭据失败：{resp}"
print("[ok] auth.set_cookie → ok（凭据不回显）")
PY_EOF

if [ -n "${XSPIDER_PROXY:-}" ]; then
  PROXY_RESP="$(rpc net.set_proxy "$(python3 -c 'import json,os; print(json.dumps({"url": os.environ["XSPIDER_PROXY"]}))')")"
  python3 - "$PROXY_RESP" <<'PY_EOF' || fail "net.set_proxy 失败"
import json, sys
resp = json.loads(sys.argv[1])
assert resp.get("result", {}).get("ok") is True, f"设置代理失败：{resp}"
print("[ok] net.set_proxy → ok")
PY_EOF
fi

USER_RESP="$(rpc fetch.get_user "$(python3 -c 'import json,sys; print(json.dumps({"screen_name": sys.argv[1]}))' "$TARGET_NAME")")"
python3 - "$USER_RESP" "$MODE_DESC" <<'PY_EOF' || fail "fetch.get_user 返回不符合契约"
import json, sys
resp = json.loads(sys.argv[1]); mode = sys.argv[2]
if "error" in resp:
    err = resp["error"]
    print(f"[!!] fetch.get_user 失败：code={err.get('code')} message={err.get('message')}", file=sys.stderr)
    print("     解析类错误 → 先存原始响应进 fixtures 再修解析；限流 → 等冷却，别重试。", file=sys.stderr)
    sys.exit(1)
user = resp["result"]["user"]
required = ["id", "screen_name", "name", "avatar"]
missing = [k for k in required if k not in user]
assert not missing, f"user 缺字段 {missing}：{user}"
assert user["id"], f"id 为空（X 改版了？）：{user}"
assert user["screen_name"], f"screen_name 为空：{user}"
assert user["avatar"].startswith("https://"), f"avatar 不像 URL：{user['avatar']}"
# media_count / register_time 允许为 null，但键必须存在（schema 要求固定形状）
assert "media_count" in user and "register_time" in user, f"固定字段缺失：{user}"
print(f"[ok] fetch.get_user（{mode}）→ id={user['id']} screen_name={user['screen_name']} media_count={user['media_count']} register_time={user['register_time']}")
PY_EOF

# net.probe_size：回放模式下**不联网**，如实回答"服务端没说"
PROBE_RESP="$(rpc net.probe_size '{"url":"https://video.twimg.com/example/clip.mp4"}')"
python3 - "$PROBE_RESP" <<'PY_EOF' || fail "net.probe_size 响应不符合契约"
import json, sys
resp = json.loads(sys.argv[1])
assert "result" in resp, f"net.probe_size 不该失败：{resp}"
assert resp["result"]["size"] is None, f"离线模式应返回 null（不联网）：{resp}"
print("[ok] net.probe_size → size=null（离线不联网）")
PY_EOF

# 未知 method 必须是结构化错误，不能是 5xx、不能是崩溃
UNKNOWN_STATUS="$(curl -sS -o "$TMP_DIR/unknown.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/" \
  -H "X-XSpider-Token: $TOKEN" -H 'Content-Type: application/json' \
  --data '{"method":"no.such.method","params":{}}')"
[ "$UNKNOWN_STATUS" = "200" ] || fail "契约错误必须用 HTTP 200 表达，实际 $UNKNOWN_STATUS"
python3 - "$TMP_DIR/unknown.json" <<'PY_EOF' || fail "未知 method 的响应形状不对"
import json, sys
resp = json.load(open(sys.argv[1]))
assert resp["error"]["code"] == "invalid_request", f"未知 method 应报 invalid_request：{resp}"
print("[ok] 未知 method → invalid_request（HTTP 200 + 结构化错误）")
PY_EOF

# 错误的 token 必须被拒
BAD_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/" \
  -H 'X-XSpider-Token: wrong-token' -H 'Content-Type: application/json' --data '{"method":"net.status","params":{}}')"
[ "$BAD_STATUS" = "401" ] || fail "错误 token 应返回 401，实际 $BAD_STATUS"
ok "错误 token → 401"

# ---------------------------------------------------------------- 5. CLI（只经契约的消费方）
step "5/6 CLI：只经契约跑一遍「取一页 → 规划 → 报告」"

# 这个二进制**不链接任何本仓库的 crate**：它的存在就是为了证明契约自己够用。
# 离线 fixture 回放 + --dry-run：不联网、不下载，CI 里能跑。

CLI_JSON="$TMP_DIR/cli-report.json"
if ! "$CLI" --screen-name "${XSPIDER_SMOKE_SCREEN_NAME:-demo_user}" \
     --fixture-dir "$ROOT/fixtures" --dry-run --json --out "$TMP_DIR/cli-out" \
     >"$CLI_JSON" 2>"$TMP_DIR/cli.stderr"; then
  cat "$TMP_DIR/cli.stderr" >&2 || true
  fail "CLI 离线 dry-run 失败"
fi
python3 - "$CLI_JSON" <<'PY_EOF' || fail "CLI 的报告不符合预期"
import json, sys
report = json.load(open(sys.argv[1]))
assert report["ok"] is True, report
assert report["mode"] == "replay", report
assert report["contract_version"], report
plan = report["plan"]
assert plan, "离线 fixture 里应当能规划出候选"
for item in plan:
    assert item["url"].startswith("https://"), item
    assert item["file_name"], item
    assert item["dest_dir"], item
# 离线不联网：大小必须如实是 null，而不是编一个数
assert all(item["size"] is None for item in plan), plan
print(f"[ok] CLI（契约 {report['contract_version']}）→ 规划 {len(plan)} 个媒体，文件名如 {plan[0]['file_name']}")
PY_EOF

# dry-run 不许写文件（目录与文件名由外壳算，但"算"不等于"建"）
[ ! -d "$TMP_DIR/cli-out" ] || fail "dry-run 不该创建输出目录"
ok "CLI dry-run 未落盘任何文件"

# ---------------------------------------------------------------- 6. 关停与残留
step "6/6 优雅退出与残留检查"
SHUTDOWN="$(rpc system.shutdown '{}')"
python3 -c 'import json,sys; assert json.loads(sys.argv[1])["result"]["ok"] is True' "$SHUTDOWN" \
  || fail "system.shutdown 响应不对：$SHUTDOWN"

for _ in $(seq 1 50); do
  kill -0 "$SIDECAR_PID" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$SIDECAR_PID" 2>/dev/null; then
  fail "system.shutdown 之后进程仍在运行（优雅退出没生效）"
fi
wait "$SIDECAR_PID" 2>/dev/null || true
SIDECAR_PID=""
ok "收到 system.shutdown 后进程已退出"

# 端口必须真的被释放（否则下次启动会撞端口）
if curl -sS --max-time 2 "http://127.0.0.1:$PORT/" -o /dev/null 2>/dev/null; then
  fail "端口 $PORT 仍在响应——进程没真正退干净"
fi
ok "端口 $PORT 已释放"

# 残留进程：docs/05 §6 明确要求断言这一条。
# **按本次运行的 token 精确匹配**：只关心"我们启动的那个进程有没有留下"，
# 不把用户在别的终端起的 sidecar 误判成残留。
# 同时检查引擎子进程（aria2next）——它最容易成为"孤儿"。
LEFTOVER="$(pgrep -f "$RUN_TOKEN" 2>/dev/null || true)"
if [ -n "$LEFTOVER" ]; then
  echo "$LEFTOVER" | while read -r pid; do ps -p "$pid" -o pid=,command= >&2 || true; done
  fail "本次运行的 xspiderd 仍有残留：$LEFTOVER"
fi
ok "本次运行的 xspiderd 无残留"

ORPHAN_ENGINE="$(pgrep -f "$RUN_TOKEN.*aria2next|aria2next.*$RUN_TOKEN" 2>/dev/null || true)"
if [ -n "$ORPHAN_ENGINE" ]; then
  fail "有孤儿 Aria2Next 进程：$ORPHAN_ENGINE"
fi
ok "无孤儿 Aria2Next 进程"

# 顺带提示（**不算失败**）：别的 xspiderd 正在跑，可能是用户自己开的
OTHER="$(pgrep -f 'xspiderd' 2>/dev/null | grep -v "^$$\$" || true)"
if [ -n "$OTHER" ]; then
  printf '[提示] 另有 %s 个 xspiderd 进程在运行（非本次运行启动，不计为残留）\n' "$(echo "$OTHER" | wc -l | tr -d ' ')"
fi

printf '\n冒烟通过：cdylib 形态可用、sidecar 握手/调用/关停/残留全部符合预期。\n'

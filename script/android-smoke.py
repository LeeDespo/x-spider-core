#!/usr/bin/env python3
"""Build a tiny no-UI APK and exercise Android nativeLibraryDir under app UID."""

from __future__ import annotations

import argparse
import os
import pathlib
import re
import shutil
import shlex
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer


ROOT = pathlib.Path(__file__).resolve().parent.parent
PACKAGE_NAME = "org.xspider.core.androidsmoke"
INSTRUMENTATION = f"{PACKAGE_NAME}/.SmokeInstrumentation"
PAYLOAD = bytes((index * 31 + 7) & 0xFF for index in range(32791))


def run(args: list[str], *, env=None, cwd=None, capture=False, check=True, timeout=120):
    try:
        result = subprocess.run(
            args,
            env=env,
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.STDOUT if capture else None,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"command timed out after {timeout}s") from None
    if check and result.returncode != 0:
        rendered = result.stdout or ""
        raise RuntimeError(f"command failed ({result.returncode}): {args!r}\n{rendered}")
    return result


def sdk_root() -> pathlib.Path:
    for raw in (
        os.environ.get("ANDROID_SDK_ROOT"),
        os.environ.get("ANDROID_HOME"),
        str(pathlib.Path.home() / "Library/Android/sdk"),
        str(pathlib.Path.home() / "Android/Sdk"),
    ):
        if raw and (pathlib.Path(raw) / "build-tools").is_dir():
            return pathlib.Path(raw)
    raise RuntimeError("Android SDK build-tools not found; set ANDROID_SDK_ROOT")


def java_home() -> pathlib.Path:
    candidates = [os.environ.get("JAVA_HOME")]
    candidates.extend(
        [
            "/Applications/Android Studio.app/Contents/jbr/Contents/Home",
            "/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home",
        ]
    )
    for raw in candidates:
        if raw and (pathlib.Path(raw) / "bin/java").is_file() and (pathlib.Path(raw) / "bin/javac").is_file():
            return pathlib.Path(raw)
    raise RuntimeError("Java/Javac not found; set JAVA_HOME (Android Studio JBR also works)")


def build_tools(sdk: pathlib.Path) -> pathlib.Path:
    versions = sorted(
        (path for path in (sdk / "build-tools").iterdir() if path.is_dir()),
        key=lambda path: tuple(int(part) if part.isdigit() else 0 for part in path.name.split(".")),
        reverse=True,
    )
    required = ("aapt", "d8", "apksigner", "zipalign")
    for path in versions:
        if all((path / name).is_file() for name in required):
            return path
    raise RuntimeError("Android SDK build-tools need aapt, d8, apksigner and zipalign")


def android_jar(sdk: pathlib.Path) -> pathlib.Path:
    candidates = []
    for path in (sdk / "platforms").glob("android-*/android.jar"):
        version = re.search(r"android-(\d+)", str(path.parent.name))
        if version:
            candidates.append((int(version.group(1)), path))
    if not candidates:
        raise RuntimeError("No Android platform android.jar found")
    return max(candidates, key=lambda item: item[0])[1]


def ndk_host_bin(ndk: pathlib.Path) -> pathlib.Path:
    if sys.platform == "darwin":
        host_names = ["darwin-arm64", "darwin-x86_64"]
    elif sys.platform.startswith("linux"):
        host_names = ["linux-x86_64"]
    else:
        raise RuntimeError(f"unsupported NDK host: {sys.platform}")
    for host in host_names:
        candidate = ndk / "toolchains/llvm/prebuilt" / host / "bin"
        if (candidate / "llvm-ar").exists():
            return candidate
    raise RuntimeError(f"NDK host tools not found below {ndk}")


def find_ndk() -> pathlib.Path:
    version = "27.3.13750724"
    candidates = [os.environ.get("ANDROID_NDK_HOME"), os.environ.get("ANDROID_NDK_ROOT")]
    for key in ("ANDROID_SDK_ROOT", "ANDROID_HOME"):
        value = os.environ.get(key)
        if value:
            candidates.append(str(pathlib.Path(value) / "ndk" / version))
    candidates.extend(
        [
            str(pathlib.Path.home() / "Library/Android/sdk/ndk" / version),
            str(pathlib.Path.home() / "Android/Sdk/ndk" / version),
        ]
    )
    for raw in candidates:
        if raw and (pathlib.Path(raw) / "source.properties").is_file():
            ndk = pathlib.Path(raw)
            source = (ndk / "source.properties").read_text()
            match = re.search(r"^Pkg\.Revision\s*=\s*(\S+)", source, re.M)
            if match and match.group(1) == version:
                return ndk
    raise RuntimeError(f"Android NDK {version} not found; set ANDROID_NDK_HOME")


def select_device(serial: str | None) -> tuple[list[str], str, int]:
    base = ["adb"]
    devices = run(base + ["devices"], capture=True).stdout.splitlines()[1:]
    online = [line.split()[0] for line in devices if len(line.split()) >= 2 and line.split()[1] == "device"]
    if serial:
        if serial not in online:
            raise RuntimeError(f"adb device {serial!r} is not online")
        selected = serial
    elif len(online) == 1:
        selected = online[0]
    elif not online:
        raise RuntimeError("no online adb device or emulator")
    else:
        raise RuntimeError(f"multiple adb devices; pass --device: {online}")
    adb = base + ["-s", selected]
    abi = run(adb + ["shell", "getprop", "ro.product.cpu.abi"], capture=True).stdout.strip()
    sdk = run(adb + ["shell", "getprop", "ro.build.version.sdk"], capture=True).stdout.strip()
    if abi not in ("arm64-v8a", "x86_64"):
        raise RuntimeError(f"device ABI {abi!r} is unsupported; smoke supports arm64-v8a/x86_64")
    return adb, abi, int(sdk)


class PayloadHandler(BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()

    def do_GET(self):
        if self.path != "/payload.bin":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        self.wfile.write(PAYLOAD)

    def log_message(self, _format, *_args):
        pass


def build_apk(package_dir: pathlib.Path, abi: str, scratch: pathlib.Path, env: dict[str, str]):
    sdk = sdk_root()
    tools = build_tools(sdk)
    platform_jar = android_jar(sdk)
    java = java_home()
    ndk = find_ndk()
    bin_dir = ndk_host_bin(ndk)
    target_prefix = "aarch64" if abi == "arm64-v8a" else "x86_64"
    clang = bin_dir / f"{target_prefix}-linux-android23-clang"
    if not clang.is_file():
        raise RuntimeError(f"API 23 NDK clang not found: {clang}")

    env = env.copy()
    env["JAVA_HOME"] = str(java)
    env["PATH"] = str(java / "bin") + os.pathsep + env.get("PATH", "")

    app_root = ROOT / "script/android-smoke-app"
    assets = scratch / "assets"
    assets.mkdir()
    shutil.copytree(package_dir / "test-fixtures/fixtures", assets / "fixtures")
    classes = scratch / "classes"
    classes.mkdir()
    java_source = app_root / "src/org/xspider/core/androidsmoke/SmokeInstrumentation.java"
    run(
        [str(java / "bin/javac"), "-source", "8", "-target", "8", "-Xlint:-options", "-bootclasspath", str(platform_jar),
         "-d", str(classes), str(java_source)],
        env=env,
    )
    dex_dir = scratch / "dex"
    dex_dir.mkdir()
    class_files = sorted(str(path) for path in classes.rglob("*.class"))
    run([str(tools / "d8"), "--min-api", "23", "--lib", str(platform_jar), "--output", str(dex_dir), *class_files], env=env)

    unsigned = scratch / "unsigned.apk"
    run(
        [str(tools / "aapt"), "package", "-f", "-M", str(app_root / "AndroidManifest.xml"),
         "-S", str(app_root / "res"), "-A", str(assets), "-I", str(platform_jar), "-F", str(unsigned)],
        env=env,
    )
    staged = scratch / "stage"
    native_dir = staged / "lib" / abi
    native_dir.mkdir(parents=True)
    shutil.copy2(package_dir / f"jniLibs/{abi}/libxspiderd.so", native_dir / "libxspiderd.so")
    shutil.copy2(package_dir / f"jniLibs/{abi}/libxspider.so", native_dir / "libxspider.so")
    check_binary = native_dir / "libxspidercheck.so"
    run(
        [str(clang), "-Wl,-z,max-page-size=16384", str(ROOT / "script/cdylib_check.c"),
         "-o", str(check_binary), "-ldl"],
        env=env,
    )
    shutil.copy2(dex_dir / "classes.dex", staged / "classes.dex")
    run(["zip", "-0", "-q", str(unsigned), "classes.dex",
         f"lib/{abi}/libxspiderd.so", f"lib/{abi}/libxspider.so", f"lib/{abi}/libxspidercheck.so"],
        env=env, cwd=staged)
    aligned = scratch / "aligned.apk"
    run([str(tools / "zipalign"), "-P", "16", "-f", "4", str(unsigned), str(aligned)], env=env)
    run([str(tools / "zipalign"), "-c", "-P", "16", "-v", "4", str(aligned)], env=env)

    keystore = scratch / "debug.keystore"
    run(
        [str(java / "bin/keytool"), "-genkeypair", "-noprompt", "-keystore", str(keystore),
         "-storepass", "android", "-keypass", "android", "-alias", "androiddebugkey", "-keyalg", "RSA",
         "-keysize", "2048", "-validity", "10000", "-dname", "CN=Android Debug,O=Android,C=US"],
        env=env,
    )
    apk = scratch / "xspider-android-smoke.apk"
    run([str(tools / "apksigner"), "sign", "--ks", str(keystore), "--ks-pass", "pass:android",
         "--key-pass", "pass:android", "--out", str(apk), str(aligned)], env=env)
    run([str(tools / "apksigner"), "verify", "--verbose", str(apk)], env=env)
    return apk


def main() -> int:
    parser = argparse.ArgumentParser(description="Build and run the no-UI Android app-UID native smoke test")
    parser.add_argument("package", nargs="?", help="android-build.sh output directory")
    parser.add_argument("--device", help="adb serial when more than one device is online")
    args = parser.parse_args()

    adb, abi, api = select_device(args.device)
    page_size = run(adb + ["shell", "getconf", "PAGE_SIZE"], capture=True, check=False).stdout.strip()
    package_dir = pathlib.Path(args.package).resolve() if args.package else (
        max(ROOT.glob(f"dist/xspiderd-*-android-{abi}"), key=lambda path: path.stat().st_mtime, default=None)
    )
    if package_dir is None:
        raise RuntimeError(f"No package found for {abi}; run script/android-build.sh {abi}")
    package_dir = pathlib.Path(package_dir)
    if not package_dir.is_dir():
        raise RuntimeError(f"Android package directory not found: {package_dir}; run script/android-build.sh {abi}")
    if not (package_dir / f"jniLibs/{abi}/libxspiderd.so").is_file():
        raise RuntimeError(f"package {package_dir} does not contain ABI {abi}")

    print(f"Device {adb[-1]}: ABI {abi}, API {api}, kernel page size {page_size or 'unknown'}; building a temporary no-UI instrumentation APK")
    print("The APK uses its own test package and is uninstalled after the smoke run.")
    if api >= 29:
        print("This app-UID test starts the PIE from ApplicationInfo.nativeLibraryDir under the Android W^X policy.")

    httpd = None
    server_thread = None
    device_port = None
    reverse_installed = False
    proxy_reverse_port = None
    proxy_reverse_installed = False
    scratch = None
    installed = False
    try:
        httpd = HTTPServer(("127.0.0.1", 0), PayloadHandler)
        server_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        server_thread.start()
        existing_reverse = run(adb + ["reverse", "--list"], capture=True).stdout.splitlines()
        mapped_ports = {
            fields[1]
            for line in existing_reverse
            if len(fields := line.split()) >= 3
        }
        proxy = None
        proxy_url = None
        if os.environ.get("XSPIDER_LIVE") == "1":
            proxy = os.environ.get("XSPIDER_ANDROID_PROXY")
            if proxy:
                proxy_url = urllib.parse.urlsplit(proxy)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            device_port = reservation.getsockname()[1]
        while f"tcp:{device_port}" in mapped_ports or (
            proxy_url and proxy_url.hostname in ("127.0.0.1", "localhost") and device_port == proxy_url.port
        ):
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                device_port = reservation.getsockname()[1]
        run(adb + ["reverse", f"tcp:{device_port}", f"tcp:{httpd.server_port}"], capture=True)
        reverse_installed = True
        download_url = f"http://127.0.0.1:{device_port}/payload.bin"

        live_args = []
        if os.environ.get("XSPIDER_LIVE") == "1":
            live_args.extend(["-e", "liveTls", "true"])
            if proxy and proxy_url:
                if proxy_url.hostname in ("127.0.0.1", "localhost") and proxy_url.port:
                    proxy_reverse_port = proxy_url.port
                    proxy_mapping = f"tcp:{proxy_reverse_port}"
                    matching = [line.split() for line in existing_reverse if len(line.split()) >= 3 and line.split()[1] == proxy_mapping]
                    if matching:
                        if matching[0][2] != proxy_mapping:
                            raise RuntimeError(f"adb reverse already maps {proxy_mapping} to {matching[0][2]}; refusing to replace it")
                    else:
                        run(adb + ["reverse", proxy_mapping, proxy_mapping], capture=True)
                        proxy_reverse_installed = True
                    proxy = f"{proxy_url.scheme}://127.0.0.1:{proxy_url.port}"
                live_args.extend(["-e", "proxy", proxy])
            print("XSPIDER_LIVE=1: the app will probe public https://x.com/robots.txt without credentials.")

        scratch_root = ROOT / "target/android-smoke-apk"
        scratch_root.mkdir(parents=True, exist_ok=True)
        scratch = pathlib.Path(tempfile.mkdtemp(prefix="run-", dir=scratch_root))
        apk = build_apk(package_dir, abi, scratch, os.environ)
        existing = run(adb + ["shell", "pm", "path", PACKAGE_NAME], capture=True, check=False).stdout.strip()
        if existing:
            raise RuntimeError(f"test package already installed; refusing to replace it: {existing}")
        run(adb + ["install", str(apk)], capture=True)
        installed = True
        run(adb + ["logcat", "-c"], capture=True)

        command = shlex.join(
            ["am", "instrument", "-w", "-e", "downloadUrl", download_url,
             "-e", "expectedBytes", str(len(PAYLOAD)), *live_args, INSTRUMENTATION]
        )
        instrumentation = run(adb + ["shell", command], capture=True, check=False, timeout=180)
        output = instrumentation.stdout or ""
        print(output, end="" if output.endswith("\n") else "\n")
        if instrumentation.returncode != 0 or "PASS:" not in output:
            logs = run(adb + ["logcat", "-d", "-s", "XSpiderAndroidSmoke:E"], capture=True, check=False)
            raise RuntimeError(f"instrumentation failed\n{logs.stdout or ''}")
        print(f"[ok] app UID {abi} smoke passed; local download was {len(PAYLOAD)} bytes")
        return 0
    finally:
        if httpd is not None:
            httpd.shutdown()
            httpd.server_close()
        if proxy_reverse_installed and proxy_reverse_port is not None:
            run(adb + ["reverse", "--remove", f"tcp:{proxy_reverse_port}"], capture=True, check=False)
        if reverse_installed and device_port is not None:
            run(adb + ["reverse", "--remove", f"tcp:{device_port}"], capture=True, check=False)
        if installed:
            run(adb + ["uninstall", PACKAGE_NAME], capture=True, check=False)
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"[!!] {error}", file=sys.stderr)
        sys.exit(1)

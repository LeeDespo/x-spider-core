package org.xspider.core.androidsmoke;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.util.Log;

import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.nio.charset.StandardCharsets;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.concurrent.FutureTask;
import java.util.concurrent.TimeUnit;

/** No-UI instrumentation check for nativeLibraryDir execution under app UID. */
public final class SmokeInstrumentation extends Instrumentation {
    private static final String TAG = "XSpiderAndroidSmoke";
    private Bundle instrumentationArguments;

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        instrumentationArguments = arguments;
        start();
    }

    @Override
    public void onStart() {
        super.onStart();
        final Bundle arguments = instrumentationArguments;
        new Thread(new Runnable() {
            @Override
            public void run() {
                runSmoke(arguments);
            }
        }, "xspider-android-smoke").start();
    }

    private void runSmoke(Bundle arguments) {
        Bundle result = new Bundle();
        Process activeProcess = null;
        try {
            Context context = getTargetContext();
            ApplicationInfo info = context.getApplicationInfo();
            File nativeDir = new File(info.nativeLibraryDir);
            File sidecar = new File(nativeDir, "libxspiderd.so");
            File cdylib = new File(nativeDir, "libxspider.so");
            File cAbiCheck = new File(nativeDir, "libxspidercheck.so");

            require(sidecar.canExecute(), "sidecar is not executable from nativeLibraryDir: " + sidecar);
            require(cdylib.isFile(), "cdylib missing from nativeLibraryDir: " + cdylib);
            require(cAbiCheck.canExecute(), "C ABI harness is not executable from nativeLibraryDir");
            publish("nativeLibraryDir exec; app uid=" + android.os.Process.myUid()
                    + "; path=" + nativeDir.getAbsolutePath());
            runProcess(context, cAbiCheck.getAbsolutePath(), cdylib.getAbsolutePath());
            publish("C ABI dlopen and calls");

            File fixtures = new File(context.getFilesDir(), "fixtures");
            copyAssetTree(context, "fixtures", fixtures);
            File state = new File(context.getFilesDir(), "state");
            File downloads = new File(context.getFilesDir(), "downloads");
            require(state.mkdirs() || state.isDirectory(), "cannot create state directory");
            require(downloads.mkdirs() || downloads.isDirectory(), "cannot create download directory");

            String downloadUrl = arguments.getString("downloadUrl");
            String expectedBytesValue = arguments.getString("expectedBytes");
            int expectedBytes = expectedBytesValue == null ? 0 : Integer.parseInt(expectedBytesValue);
            require(downloadUrl != null && downloadUrl.startsWith("http://"), "downloadUrl argument missing");
            require(expectedBytes > 0, "expectedBytes argument missing");

            Process sidecarProcess = new ProcessBuilder(
                    sidecar.getAbsolutePath(),
                    "--port", "0",
                    "--fixture-dir", fixtures.getAbsolutePath(),
                    "--state-dir", state.getAbsolutePath())
                    .start();
            activeProcess = sidecarProcess;
            ErrorCollector diagnostics = new ErrorCollector(sidecarProcess.getErrorStream());
            diagnostics.start();
            final BufferedReader readyReader = new BufferedReader(new InputStreamReader(
                    sidecarProcess.getInputStream(), StandardCharsets.UTF_8));
            FutureTask<String> readyLineTask = new FutureTask<>(new java.util.concurrent.Callable<String>() {
                @Override
                public String call() throws Exception {
                    return readyReader.readLine();
                }
            });
            new Thread(readyLineTask, "xspider-sidecar-ready").start();
            String readyLine = readyLineTask.get(15, TimeUnit.SECONDS);
            require(readyLine != null && readyLine.startsWith("ready "),
                    "sidecar did not print a valid ready handshake; stderr=" + diagnostics.contents());
            JSONObject ready = new JSONObject(readyLine.substring("ready ".length()));
            require("1.5.2".equals(ready.getString("version")),
                    "ready handshake returned unexpected contract version: " + ready.getString("version"));
            require(!ready.getString("token").isEmpty(), "ready handshake did not provide a token");
            String endpoint = "http://127.0.0.1:" + ready.getInt("port") + "/";
            HttpRpc rpc = new HttpRpc(endpoint, ready.getString("token"));

            JSONObject version = rpc.call("system.version", new JSONObject());
            require("1.5.2".equals(version.getString("contract_version")),
                    "unexpected contract version: " + version);
            publish("HTTP ready/token handshake and system.version");

            JSONObject userArgs = new JSONObject().put("screen_name", "demo_user");
            rpc.call("auth.set_cookie", new JSONObject().put("cookie", "auth_token=fixture; ct0=fixture"));
            JSONObject userResult = rpc.call("fetch.get_user", userArgs);
            String screenName = userResult.getJSONObject("user").getString("screen_name");
            require("demo_user".equals(screenName), "fixture fetch returned " + screenName);
            publish("HTTP authorized fixture fetch.get_user");

            File destination = new File(downloads, "payload.bin");
            String jobId = "android-app-smoke-" + System.currentTimeMillis();
            JSONObject enqueue = new JSONObject()
                    .put("job_id", jobId)
                    .put("url", downloadUrl)
                    .put("dest_dir", downloads.getAbsolutePath())
                    .put("file_name", destination.getName())
                    .put("expect_size", expectedBytes);
            rpc.call("dl.enqueue", enqueue);
            publish("HTTP authorized dl.enqueue");

            long deadline = System.currentTimeMillis() + 30000;
            JSONObject job = null;
            while (System.currentTimeMillis() < deadline) {
                job = rpc.call("dl.status", new JSONObject().put("job_id", jobId))
                        .getJSONObject("job");
                String stateName = job.getString("state");
                if ("complete".equals(stateName)) {
                    break;
                }
                if ("error".equals(stateName)) {
                    throw new IllegalStateException("download failed: " + job);
                }
                Thread.sleep(150);
            }
            require(job != null && "complete".equals(job.getString("state")),
                    "download did not complete: " + job);
            verifyPayload(destination, expectedBytes);
            publish("verified local HTTP download bytes");

            rpc.call("system.shutdown", new JSONObject());
            require(waitFor(sidecarProcess, 10_000), "HTTP sidecar did not exit after system.shutdown");
            require(sidecarProcess.exitValue() == 0,
                    "HTTP sidecar exited " + sidecarProcess.exitValue() + ": " + diagnostics.contents());
            activeProcess = null;

            File stdioState = new File(context.getFilesDir(), "stdio-state");
            require(stdioState.mkdirs() || stdioState.isDirectory(), "cannot create stdio state directory");
            Process stdioProcess = new ProcessBuilder(
                    sidecar.getAbsolutePath(), "--stdio",
                    "--fixture-dir", fixtures.getAbsolutePath(),
                    "--state-dir", stdioState.getAbsolutePath()).start();
            activeProcess = stdioProcess;
            ErrorCollector stdioDiagnostics = new ErrorCollector(stdioProcess.getErrorStream());
            stdioDiagnostics.start();
            BufferedWriter stdioRequests = new BufferedWriter(new OutputStreamWriter(
                    stdioProcess.getOutputStream(), StandardCharsets.UTF_8));
            BufferedReader stdioResponses = new BufferedReader(new InputStreamReader(
                    stdioProcess.getInputStream(), StandardCharsets.UTF_8));
            Rpc stdioRpc = new Rpc(stdioRequests, stdioResponses, stdioDiagnostics);
            JSONObject stdioVersion = stdioRpc.call("system.version", new JSONObject());
            require("1.5.2".equals(stdioVersion.getString("contract_version")),
                    "stdio transport returned unexpected version: " + stdioVersion);
            stdioRpc.call("auth.set_cookie", new JSONObject().put("cookie", "auth_token=fixture; ct0=fixture"));
            JSONObject stdioUser = stdioRpc.call("fetch.get_user", userArgs);
            require("demo_user".equals(stdioUser.getJSONObject("user").getString("screen_name")),
                    "stdio fixture fetch returned unexpected user: " + stdioUser);
            stdioRequests.close();
            require(waitFor(stdioProcess, 10_000), "stdio sidecar did not exit after stdin EOF");
            require(stdioProcess.exitValue() == 0,
                    "stdio sidecar exited " + stdioProcess.exitValue() + ": " + stdioDiagnostics.contents());
            activeProcess = null;
            publish("stdio system.version and fixture fetch");

            if (Boolean.parseBoolean(arguments.getString("liveTls"))) {
                File liveState = new File(context.getFilesDir(), "live-state");
                require(liveState.mkdirs() || liveState.isDirectory(), "cannot create live state directory");
                String proxy = arguments.getString("proxy");
                Process liveSidecar = startLiveSidecar(sidecar, liveState, proxy);
                activeProcess = liveSidecar;
                ErrorCollector liveDiagnostics = new ErrorCollector(liveSidecar.getErrorStream());
                liveDiagnostics.start();
                BufferedWriter liveRequests = new BufferedWriter(new OutputStreamWriter(
                        liveSidecar.getOutputStream(), StandardCharsets.UTF_8));
                BufferedReader liveResponses = new BufferedReader(new InputStreamReader(
                        liveSidecar.getInputStream(), StandardCharsets.UTF_8));
                Rpc liveRpc = new Rpc(liveRequests, liveResponses, liveDiagnostics);
                JSONObject probe = liveRpc.call("net.probe_size",
                        new JSONObject().put("url", "https://x.com/robots.txt"));
                require(probe.has("size"), "successful public robots probe omitted its size field");
                String sizeEvidence = probe.isNull("size")
                        ? "size=null (TLS/HTTP request succeeded; server advertised no length)"
                        : "size=" + probe.getLong("size");
                if (!probe.isNull("size")) {
                    require(probe.getLong("size") > 0, "public robots probe returned a non-positive size");
                }
                liveRequests.close();
                require(waitFor(liveSidecar, 90_000), "live TLS sidecar did not exit");
                require(liveSidecar.exitValue() == 0,
                        "public robots TLS probe failed: " + liveDiagnostics.contents());
                activeProcess = null;
                publish("live public robots TLS/HTTP probe (no cookie), " + sizeEvidence);
            }
            result.putString("stream", "PASS: app-UID HTTP/stdio sidecars, C ABI, fixture fetch, local download");
            result.putString("nativeLibraryDir", nativeDir.getAbsolutePath());
            result.putInt("uid", android.os.Process.myUid());
            Log.i(TAG, result.getString("stream"));
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("stream", "FAIL: " + error);
            Log.e(TAG, result.getString("stream"), error);
            finish(Activity.RESULT_CANCELED, result);
        } finally {
            stopProcess(activeProcess);
        }
    }

    private void publish(String stage) {
        Bundle status = new Bundle();
        status.putString("stage", stage);
        sendStatus(1, status);
    }

    private static void verifyPayload(File file, int expectedBytes) throws Exception {
        require(file.isFile(), "download file missing: " + file);
        byte[] actual = new byte[expectedBytes];
        try (InputStream input = new FileInputStream(file)) {
            int offset = 0;
            while (offset < actual.length) {
                int count = input.read(actual, offset, actual.length - offset);
                if (count < 0) {
                    break;
                }
                offset += count;
            }
            require(offset == expectedBytes, "download length mismatch: " + offset);
            require(input.read() == -1, "download contains trailing bytes");
        }
        for (int index = 0; index < actual.length; index++) {
            int expected = (index * 31 + 7) & 0xff;
            require((actual[index] & 0xff) == expected,
                    "download content mismatch at byte " + index);
        }
    }

    private void copyAssetTree(Context context, String source, File destination) throws Exception {
        String[] children = context.getAssets().list(source);
        if (children != null && children.length > 0) {
            require(destination.mkdirs() || destination.isDirectory(), "cannot create " + destination);
            for (String child : children) {
                copyAssetTree(context, source + "/" + child, new File(destination, child));
            }
            return;
        }
        File parent = destination.getParentFile();
        require(parent != null && (parent.mkdirs() || parent.isDirectory()), "cannot create asset parent");
        try (InputStream input = context.getAssets().open(source);
             OutputStream output = new FileOutputStream(destination)) {
            byte[] buffer = new byte[8192];
            int count;
            while ((count = input.read(buffer)) >= 0) {
                output.write(buffer, 0, count);
            }
        }
    }

    private void runProcess(Context context, String... command) throws Exception {
        File outputFile = new File(context.getFilesDir(), "cabi-check.log");
        Process process = new ProcessBuilder(command)
                .redirectErrorStream(true)
                .redirectOutput(outputFile)
                .start();
        if (!waitFor(process, 20_000)) {
            process.destroy();
            throw new IllegalStateException("native harness timed out");
        }
        StringBuilder rendered = new StringBuilder();
        try (BufferedReader output = new BufferedReader(new InputStreamReader(
                new FileInputStream(outputFile), StandardCharsets.UTF_8))) {
            String line;
            while ((line = output.readLine()) != null) {
                rendered.append(line).append('\n');
            }
        }
        require(process.exitValue() == 0, "native harness failed: " + rendered);
        Log.i(TAG, "C ABI harness: " + rendered.toString().trim());
    }

    private Process startLiveSidecar(File sidecar, File state, String proxy) throws Exception {
        ProcessBuilder builder = new ProcessBuilder(sidecar.getAbsolutePath(), "--stdio",
                "--state-dir", state.getAbsolutePath());
        builder.environment().remove("XSPIDER_COOKIE");
        builder.environment().remove("XSPIDER_COOKIE_FILE");
        if (proxy != null && !proxy.isEmpty()) {
            builder.environment().put("XSPIDER_PROXY", proxy);
        } else {
            builder.environment().remove("XSPIDER_PROXY");
        }
        return builder.start();
    }

    /** Process.waitFor(timeout, unit) was added in API 26; this smoke APK supports API 23. */
    private static boolean waitFor(Process process, long timeoutMs) throws InterruptedException {
        long deadline = System.currentTimeMillis() + timeoutMs;
        while (System.currentTimeMillis() < deadline) {
            try {
                process.exitValue();
                return true;
            } catch (IllegalThreadStateException stillRunning) {
                Thread.sleep(100);
            }
        }
        try {
            process.exitValue();
            return true;
        } catch (IllegalThreadStateException stillRunning) {
            return false;
        }
    }

    private static void stopProcess(Process process) {
        if (process == null) {
            return;
        }
        try {
            process.exitValue();
            return;
        } catch (IllegalThreadStateException stillRunning) {
            process.destroy();
        }
        try {
            waitFor(process, 5_000);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) {
            throw new IllegalStateException(message);
        }
    }

    private static final class Rpc {
        private final BufferedWriter requests;
        private final BufferedReader responses;
        private final ErrorCollector diagnostics;
        private int nextId = 1;

        Rpc(BufferedWriter requests, BufferedReader responses, ErrorCollector diagnostics) {
            this.requests = requests;
            this.responses = responses;
            this.diagnostics = diagnostics;
        }

        JSONObject call(String method, JSONObject params) throws Exception {
            int id = nextId++;
            JSONObject request = new JSONObject()
                    .put("id", id)
                    .put("method", method)
                    .put("params", params);
            requests.write(request.toString());
            requests.newLine();
            requests.flush();
            String line = responses.readLine();
            if (line == null) {
                throw new IllegalStateException("sidecar closed stdio; stderr=" + diagnostics.contents());
            }
            JSONObject response = new JSONObject(line);
            if (response.has("error")) {
                throw new IllegalStateException(method + " returned " + response.getJSONObject("error"));
            }
            return response.getJSONObject("result");
        }
    }

    private static final class HttpRpc {
        private final URL endpoint;
        private final String token;
        private int nextId = 1;

        HttpRpc(String endpoint, String token) throws Exception {
            this.endpoint = new URL(endpoint);
            this.token = token;
        }

        JSONObject call(String method, JSONObject params) throws Exception {
            HttpURLConnection connection = (HttpURLConnection) endpoint.openConnection();
            connection.setRequestMethod("POST");
            connection.setConnectTimeout(10_000);
            connection.setReadTimeout(20_000);
            connection.setDoOutput(true);
            connection.setRequestProperty("Content-Type", "application/json");
            connection.setRequestProperty("X-XSpider-Token", token);
            JSONObject request = new JSONObject()
                    .put("id", nextId++)
                    .put("method", method)
                    .put("params", params);
            byte[] body = request.toString().getBytes(StandardCharsets.UTF_8);
            connection.setFixedLengthStreamingMode(body.length);
            try (OutputStream output = connection.getOutputStream()) {
                output.write(body);
            }
            int status = connection.getResponseCode();
            InputStream stream = status >= 400 ? connection.getErrorStream() : connection.getInputStream();
            require(stream != null, "HTTP RPC returned no body, status=" + status);
            StringBuilder rendered = new StringBuilder();
            try (BufferedReader reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))) {
                String line;
                while ((line = reader.readLine()) != null) {
                    rendered.append(line);
                }
            } finally {
                connection.disconnect();
            }
            JSONObject response = new JSONObject(rendered.toString());
            require(status == 200, method + " HTTP status " + status + ": " + response);
            if (response.has("error")) {
                throw new IllegalStateException(method + " returned " + response.getJSONObject("error"));
            }
            return response.getJSONObject("result");
        }
    }

    private static final class ErrorCollector extends Thread {
        private final BufferedReader reader;
        private final StringBuilder output = new StringBuilder();

        ErrorCollector(InputStream stream) {
            super("xspider-sidecar-stderr");
            reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8));
            setDaemon(true);
        }

        @Override
        public void run() {
            try {
                String line;
                while ((line = reader.readLine()) != null) {
                    synchronized (output) {
                        output.append(line).append('\n');
                    }
                }
            } catch (Exception ignored) {
            }
        }

        String contents() {
            synchronized (output) {
                return output.toString();
            }
        }
    }
}

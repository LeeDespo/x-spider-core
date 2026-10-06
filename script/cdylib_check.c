/*
 * 最小 C 程序：dlopen 生成的 cdylib，调用三个 C ABI 函数。
 *
 * 为什么必须真有这一步（docs/05-WORKFLOW.md 的命令手册/环境初始化；M0 验收 ① 见 docs/ROADMAP.md 的 M0 节）：
 * 「cdylib 能编译出来」和「cdylib 能被别的语言真的加载并调用」是两件事。
 * 少了这一步，次形态是否可用完全靠运气——而它的失败方式是**静默**的
 * （未签名 / 被 quarantine 的产物会被内核 SIGKILL，见 docs/03 §1）。
 *
 * 用法：cc -o cdylib_check script/cdylib_check.c && ./cdylib_check <路径到 libxspider.dylib>
 *
 * 只依赖 dlfcn.h / stdio.h / string.h / stdlib.h（POSIX 与 Windows 都有对应物）。
 */

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef char *(*version_fn)(void);
typedef char *(*call_fn)(const char *, const char *);
typedef void (*free_fn)(char *);

static int failures = 0;

static void check(int ok, const char *what) {
    printf("  %s %s\n", ok ? "[ok]" : "[!!]", what);
    if (!ok) {
        failures++;
    }
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "用法：%s <cdylib 路径>\n", argv[0]);
        return 2;
    }
    const char *path = argv[1];

    /* RTLD_NOW：符号缺失立刻失败，不要拖到第一次调用 */
    void *handle = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (handle == NULL) {
        fprintf(stderr, "dlopen 失败：%s\n", dlerror());
        fprintf(stderr,
                "提示：macOS 上未签名 / 带 quarantine 的 dylib 会加载失败。\n"
                "      xattr -dr com.apple.quarantine %s\n"
                "      codesign -f -s - %s\n",
                path, path);
        return 1;
    }
    printf("dlopen 成功：%s\n", path);

    version_fn fn_version = (version_fn)dlsym(handle, "xspider_version");
    call_fn fn_call = (call_fn)dlsym(handle, "xspider_call");
    free_fn fn_free = (free_fn)dlsym(handle, "xspider_free");

    check(fn_version != NULL, "dlsym xspider_version");
    check(fn_call != NULL, "dlsym xspider_call");
    check(fn_free != NULL, "dlsym xspider_free");
    if (failures) {
        dlclose(handle);
        return 1;
    }

    /* 1) 契约版本握手 */
    char *version = fn_version();
    check(version != NULL, "xspider_version 返回非 NULL");
    if (version != NULL) {
        printf("       contract_version = %s\n", version);
        check(strlen(version) > 0, "version 非空");
        fn_free(version);
    }

    /* 2) 一次真实派发 */
    char *out = fn_call("system.version", "{}");
    check(out != NULL, "xspider_call 返回非 NULL（绝不能返回 NULL）");
    if (out != NULL) {
        printf("       system.version -> %s\n", out);
        check(strstr(out, "\"contract_version\"") != NULL, "响应里有 contract_version");
        check(strstr(out, "\"result\"") != NULL, "成功响应用 result 包络");
        fn_free(out);
    }

    /* 3) 未知 method 必须是结构化错误，不是崩溃、不是空串 */
    out = fn_call("does.not.exist", "{}");
    check(out != NULL, "未知 method 仍返回字符串");
    if (out != NULL) {
        printf("       未知 method -> %s\n", out);
        check(strstr(out, "\"invalid_request\"") != NULL, "未知 method 报 invalid_request");
        fn_free(out);
    }

    /* 4) 非法 JSON 与空指针都不能把它打崩 */
    out = fn_call("system.version", "{not json");
    check(out != NULL && strstr(out, "invalid_request") != NULL, "非法 JSON → invalid_request");
    if (out != NULL) {
        fn_free(out);
    }
    out = fn_call(NULL, NULL);
    check(out != NULL && strstr(out, "invalid_request") != NULL, "空指针 → invalid_request");
    if (out != NULL) {
        fn_free(out);
    }
    fn_free(NULL); /* 释放 NULL 必须是安全的空操作 */
    check(1, "xspider_free(NULL) 不崩溃");

    dlclose(handle);

    if (failures) {
        printf("cdylib 形态检查失败：%d 项\n", failures);
        return 1;
    }
    printf("cdylib 形态检查通过\n");
    return 0;
}

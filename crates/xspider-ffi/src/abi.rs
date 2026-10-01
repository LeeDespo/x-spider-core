//! C ABI 层。**只有三个函数**，这是唯一的跨编译器、跨语言稳定接口面
//! （`docs/03-FFI-SIGNING-PACKAGING.md` §2）。
//!
//! ```c
//! char* xspider_version(void);
//! char* xspider_call(const char* method, const char* json_in);
//! void  xspider_free(char* ptr);
//! ```
//!
//! ## 所有权
//!
//! `xspider_version` / `xspider_call` 返回的字符串**由调用方负责**用 `xspider_free` 释放。
//! 用别的 free（包括系统 free）释放是未定义行为。
//!
//! ## 三条硬性要求
//!
//! 1. **绝不 panic 穿过 FFI 边界**：所有入口都包在 `catch_unwind` 里，
//!    panic 被转成结构化的 `internal` 错误。跨边界展开是 UB，而且会让 Swift 侧直接崩。
//! 2. **绝不返回 NULL**：调用方永远拿到一段可解析的 JSON；
//!    连"引擎构造失败"这种最坏情况也返回 `{"error":{...}}`。
//! 3. **参数一律当不可信**：空指针、非 UTF-8、非法 JSON、未知 method 全部有明确处理。

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use serde_json::Value;
use tokio::runtime::{Builder, Runtime};

use xspider_core::error::{err_envelope_string, ok_envelope, XError};
use xspider_core::CONTRACT_VERSION;

use crate::engine::{engine_from_env, Engine};

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

fn runtime() -> Result<&'static Runtime, String> {
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .enable_all()
                .thread_name("xspider")
                .build()
                .map_err(|e| format!("构造 tokio 运行时失败：{e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

fn engine() -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| engine_from_env().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| e.clone())
}

/// 把一段字符串转成 C 侧拥有的 `char*`。
fn into_c_string(s: String) -> *mut c_char {
    // 内部字符串理论上不含 NUL；真出现了也不能让整个调用失败
    let sanitized = s.replace('\0', " ");
    match CString::new(sanitized) {
        Ok(c) => c.into_raw(),
        Err(_) => {
            let fallback = err_envelope_string(&XError::internal("内部错误：响应含 NUL 字节"));
            CString::new(fallback)
                .expect("兜底字符串一定合法")
                .into_raw()
        }
    }
}

/// 契约版本握手。返回如 `"1.0.0"`。
///
/// 外壳启动时比对；**不匹配就拒绝启动并给明确提示**，不要降级到"部分功能可用"
/// （`docs/01-ARCHITECTURE.md` §9）。
///
/// # Safety
/// 返回值必须交给 [`xspider_free`] 释放。
#[no_mangle]
pub extern "C" fn xspider_version() -> *mut c_char {
    let version =
        catch_unwind(|| CONTRACT_VERSION.to_string()).unwrap_or_else(|_| "0.0.0-panic".to_string());
    into_c_string(version)
}

/// 所有能力的唯一入口。返回 `{"result": ...}` 或 `{"error": ...}`。
///
/// # Safety
/// - `method` 必须是 NUL 结尾的合法 UTF-8 C 字符串，可为 NULL（→ 结构化错误）；
/// - `json_in` **可以是** NULL 或空串（等价于 `{}`），用于无参数的方法；
/// - 返回值必须交给 [`xspider_free`] 释放。
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // 这三个函数的指针参数由调用方保证，见上面的 Safety
pub extern "C" fn xspider_call(method: *const c_char, json_in: *const c_char) -> *mut c_char {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let method = match read_c_str(method, "method") {
            Ok(m) => m,
            Err(e) => return err_envelope_string(&e),
        };
        // `json_in` 允许为 NULL / 空串，等价于 `{}`：
        // 无参数的方法（如 `net.status`）在 C 侧传 NULL 是最自然的写法，
        // 为此报错属于为难调用方。`method` 则不接受 NULL——那一定是调用方的 bug。
        let json_in = read_optional_c_str(json_in);
        dispatch(&method, &json_in)
    }));
    match outcome {
        Ok(s) => into_c_string(s),
        // panic 绝不能穿过 FFI 边界
        Err(_) => into_c_string(err_envelope_string(&XError::internal(
            "内部 panic（已拦截）；请附日志报告这个 bug",
        ))),
    }
}

/// 释放 [`xspider_version`] / [`xspider_call`] 返回的字符串。
///
/// # Safety
/// `ptr` 必须来自本库，且只能释放一次。传 NULL 是安全的空操作。
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn xspider_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // 忽略 panic：析构一个合法 CString 不会失败
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(CString::from_raw(ptr));
    }));
}

/// 可缺省的入参：NULL 视为空串。
fn read_optional_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // 非 UTF-8 时退化成空串而不是报错：调用方传的是"没有参数"，
    // 我们不该因为一段本来就没用的字节而拒绝整个调用。
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .unwrap_or_default()
        .to_string()
}

fn read_c_str(ptr: *const c_char, field: &str) -> Result<String, XError> {
    if ptr.is_null() {
        return Err(XError::invalid_request(format!(
            "{field} 是空指针（不能为 NULL）"
        )));
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str()
        .map(str::to_string)
        .map_err(|e| XError::invalid_request(format!("{field} 不是合法 UTF-8：{e}")))
}

fn dispatch(method: &str, json_in: &str) -> String {
    let trimmed = json_in.trim();
    let params: Value = if trimmed.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return err_envelope_string(&XError::invalid_request(format!(
                    "json_in 不是合法 JSON：{e}"
                )))
            }
        }
    };

    let rt = match runtime() {
        Ok(rt) => rt,
        Err(message) => return err_envelope_string(&XError::internal(message)),
    };
    let engine = match engine() {
        Ok(engine) => engine,
        Err(message) => {
            return err_envelope_string(&XError::internal(format!("引擎初始化失败：{message}")))
        }
    };

    match rt.block_on(engine.call(method, &params)) {
        Ok(value) => ok_envelope(value).to_string(),
        Err(e) => err_envelope_string(&e),
    }
}

/// 供测试用：不经 C ABI 直接跑一次派发（返回 JSON 字符串）。
///
/// 放在这里是为了让契约测试能同时覆盖"库形态的 C ABI"与"库形态的 Rust API"。
pub fn call_as_json(method: &str, json_in: &str) -> String {
    catch_unwind(AssertUnwindSafe(|| dispatch(method, json_in)))
        .unwrap_or_else(|_| err_envelope_string(&XError::internal("panic（已拦截）")))
}

/// 供测试与排障：把 `{"error":...}`/`{"result":...}` 包络拆成两个可选值。
pub fn envelope_parts(value: &Value) -> (Option<&Value>, Option<&Value>) {
    (value.get("result"), value.get("error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use xspider_core::error::err_envelope;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("xspider_call 的输出必须是合法 JSON")
    }

    #[test]
    fn version_is_non_empty_and_freed_safely() {
        let ptr = xspider_version();
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        assert_eq!(s, CONTRACT_VERSION);
        xspider_free(ptr);
        // 释放 NULL 必须是安全的空操作
        xspider_free(std::ptr::null_mut());
    }

    #[test]
    fn unknown_method_returns_error_envelope_not_null() {
        let method = CString::new("nope").unwrap();
        let params = CString::new("{}").unwrap();
        let ptr = xspider_call(method.as_ptr(), params.as_ptr());
        assert!(!ptr.is_null());
        let v = parse(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap());
        xspider_free(ptr);
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(v.get("result").is_none());
    }

    #[test]
    fn null_method_is_rejected_structurally() {
        let params = CString::new("{}").unwrap();
        let ptr = xspider_call(std::ptr::null(), params.as_ptr());
        let v = parse(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap());
        xspider_free(ptr);
        assert_eq!(v["error"]["code"], "invalid_request");
    }

    #[test]
    fn null_or_empty_params_mean_empty_object() {
        let method = CString::new("system.version").unwrap();
        for ptr_in in [std::ptr::null(), CString::new("").unwrap().as_ptr()] {
            let ptr = xspider_call(method.as_ptr(), ptr_in);
            let v = parse(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap());
            xspider_free(ptr);
            assert_eq!(v["result"]["contract_version"], CONTRACT_VERSION, "{v}");
        }
    }

    #[test]
    fn malformed_json_is_rejected_with_a_clear_message() {
        let method = CString::new("fetch.get_user").unwrap();
        let params = CString::new("{not json").unwrap();
        let ptr = xspider_call(method.as_ptr(), params.as_ptr());
        let v = parse(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap());
        xspider_free(ptr);
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(
            v["error"]["message"].as_str().unwrap().contains("JSON"),
            "{v}"
        );
    }

    #[test]
    fn invalid_utf8_method_is_rejected_not_ub() {
        // 0xFF 不是合法 UTF-8
        let bad = [0xFFu8, 0x00];
        let params = CString::new("{}").unwrap();
        let ptr = xspider_call(bad.as_ptr() as *const c_char, params.as_ptr());
        let v = parse(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap());
        xspider_free(ptr);
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(v["error"]["message"].as_str().unwrap().contains("UTF-8"));
    }

    #[test]
    fn envelope_helper_splits_both_shapes() {
        let ok = ok_envelope(json!({ "a": 1 }));
        let (result, error) = envelope_parts(&ok);
        assert_eq!(result.unwrap()["a"], 1);
        assert!(error.is_none());

        let err = err_envelope(&XError::Cancelled);
        let (result, error) = envelope_parts(&err);
        assert!(result.is_none());
        assert_eq!(error.unwrap()["code"], "cancelled");
    }
}

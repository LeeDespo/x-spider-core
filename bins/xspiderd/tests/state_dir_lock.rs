//! 环境变量与 flag 都保护记录目录；同时启动的第二个 sidecar 必须被拒绝。
#![cfg(unix)]
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn env_state_dir_is_exclusive_and_released_after_process_death() {
    let dir = std::env::temp_dir().join(format!("xspider-state-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut first = Command::new(env!("CARGO_BIN_EXE_xspiderd"))
        .args(["--port", "0", "--fixture-dir"])
        .arg(&fixtures)
        .env("XSPIDER_STATE_DIR", &dir)
        .env_remove("XSPIDER_COOKIE")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(first.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.starts_with("ready "), "first sidecar did not start");
    let mut second = Command::new(env!("CARGO_BIN_EXE_xspiderd"))
        .args(["--port", "0", "--fixture-dir"])
        .arg(&fixtures)
        .arg("--state-dir")
        .arg(&dir)
        .env_remove("XSPIDER_COOKIE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            second.kill().unwrap();
            second.wait().unwrap();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(
        status.is_some_and(|s| !s.success()),
        "env and flag state dirs must share the same exclusive lock"
    );
    // A new instance can start after SIGKILL: the kernel releases the file lock.
    let mut third = Command::new(env!("CARGO_BIN_EXE_xspiderd"))
        .args(["--port", "0", "--fixture-dir"])
        .arg(&fixtures)
        .env("XSPIDER_STATE_DIR", &dir)
        .env_remove("XSPIDER_COOKIE")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(third.stdout.take().unwrap());
    line.clear();
    stdout.read_line(&mut line).unwrap();
    third.kill().unwrap();
    third.wait().unwrap();
    assert!(line.starts_with("ready "));
    std::fs::remove_dir_all(dir).unwrap();
}

//! 单实例锁（`docs/03-FFI-SIGNING-PACKAGING.md` §3）。
//!
//! 为什么需要：**多实例必须各自独立**（独立端口 + 独立 state-dir），
//! 否则两个进程会拿着同一个 state-dir 互踩。
//!
//! 失效方向是刻意选的：**宁可放行，也不要把启动卡住**。
//! 锁文件存在但持有者进程已不在（崩溃/被 kill）→ 直接接管并覆盖，
//! 这样"上次崩了导致这次起不来"不会发生。

use std::io::Write;
use std::path::{Path, PathBuf};

pub struct InstanceLock {
    path: PathBuf,
}

impl InstanceLock {
    /// 在 `state_dir` 下取得独占锁。已有**活着**的同名实例时返回 `Err`。
    pub fn acquire(state_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| format!("创建 state-dir {} 失败：{e}", state_dir.display()))?;
        let path = state_dir.join("xspiderd.lock");

        if let Ok(existing) = std::fs::read_to_string(&path) {
            if let Ok(pid) = existing.trim().parse::<u32>() {
                if pid != std::process::id() && process_is_alive(pid) {
                    return Err(format!(
                        "state-dir {} 已被另一个实例占用（pid {pid}）。\
                         多实例请各自使用独立的 --state-dir 与端口（docs/03 §3）。",
                        state_dir.display()
                    ));
                }
                tracing::warn!(pid, "发现残留的锁文件（持有者已不在），接管它");
            }
        }

        let mut file = std::fs::File::create(&path)
            .map_err(|e| format!("写入锁文件 {} 失败：{e}", path.display()))?;
        writeln!(file, "{}", std::process::id())
            .map_err(|e| format!("写入锁文件 {} 失败：{e}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // 只删自己的锁：如果被别人接管过，这个文件已经不属于我们了
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.trim() == std::process::id().to_string() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `kill -0`：只做权限/存在性检查，不真的发信号。
    // 用系统自带工具而不是引入 libc：M0 阶段不值得为一个探测加一个 crate。
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    // 非 unix 上没办法可靠探测，于是选择"当作已死"——
    // 宁可让两个实例并存（各自独立 state-dir 时本来就安全），也不要卡住启动。
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xspider-lock-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn second_instance_is_refused_while_a_live_one_holds_the_lock() {
        use std::os::unix::process::parent_id;
        let dir = temp_dir("live");
        let lock = InstanceLock::acquire(&dir).unwrap();
        // 伪造"另一个活着的进程持有锁"：用父进程（cargo/测试宿主）的 pid。
        // 不能用当前进程自己的 pid——那个值语义上表示"同一进程留下的锁"，
        // 按设计应当被接管而不是拒绝。
        std::fs::write(&lock.path, format!("{}\n", parent_id())).unwrap();
        let second = InstanceLock::acquire(&dir);
        assert!(second.is_err(), "两个活实例不该同时拿到锁");
        drop(lock);
    }

    #[test]
    fn own_pid_in_the_lock_is_taken_over_not_refused() {
        // 同一进程重复获取（例如测试里连续构造）不该自我死锁
        let dir = temp_dir("own");
        let lock = InstanceLock::acquire(&dir).unwrap();
        std::fs::write(&lock.path, format!("{}\n", std::process::id())).unwrap();
        assert!(InstanceLock::acquire(&dir).is_ok());
    }

    #[test]
    fn stale_lock_is_taken_over_instead_of_blocking_startup() {
        let dir = temp_dir("stale");
        // pid 1 在容器里可能存在，这里用一个几乎不可能存在的 pid
        std::fs::write(dir.join("xspiderd.lock"), "4194303999\n").unwrap();
        let lock = InstanceLock::acquire(&dir).expect("残留锁必须被接管，否则崩一次就起不来");
        let content = std::fs::read_to_string(&lock.path).unwrap();
        assert_eq!(content.trim(), std::process::id().to_string());
    }

    #[test]
    fn drop_removes_the_lock_file() {
        let dir = temp_dir("drop");
        let path = {
            let lock = InstanceLock::acquire(&dir).unwrap();
            lock.path.clone()
        };
        assert!(!path.exists(), "退出后不该留下锁文件");
    }

    #[test]
    fn drop_does_not_remove_someone_elses_lock() {
        let dir = temp_dir("someone-else");
        let lock = InstanceLock::acquire(&dir).unwrap();
        let path = lock.path.clone();
        std::fs::write(&path, "12345\n").unwrap();
        drop(lock);
        assert!(path.exists(), "被别人接管过的锁不该被我们删掉");
    }
}

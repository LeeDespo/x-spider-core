//! 状态目录的进程互斥。Unix 使用内核文件锁，崩溃/被系统杀死也会自动释放。
//! 锁文件保留在目录里：释放时删除会使等待者与新进程锁住不同 inode。

use std::io::Write;
use std::path::Path;
#[cfg(any(not(unix), test))]
use std::path::PathBuf;

pub struct InstanceLock {
    #[cfg(unix)]
    _file: std::fs::File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(state_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| format!("创建 state-dir {} 失败：{e}", state_dir.display()))?;
        let path = state_dir.join("xspiderd.lock");
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|e| format!("打开锁文件 {} 失败：{e}", path.display()))?;
            // SAFETY: file 在 InstanceLock 的整个生命周期内保持打开；flock 不访问指针。
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(format!(
                    "state-dir {} 无法取得独占锁：{}。多实例请使用独立状态目录。",
                    state_dir.display(),
                    std::io::Error::last_os_error()
                ));
            }
            file.set_len(0)
                .map_err(|e| format!("更新锁文件失败：{e}"))?;
            writeln!(file, "{}", std::process::id()).map_err(|e| format!("写入锁文件失败：{e}"))?;
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            // 非 Unix 保留原来的启动路径；尚不声称此路径具有跨进程互斥保证。
            let mut file = std::fs::File::create(&path)
                .map_err(|e| format!("写入锁文件 {} 失败：{e}", path.display()))?;
            writeln!(file, "{}", std::process::id()).map_err(|e| format!("写入锁文件失败：{e}"))?;
            Ok(Self { path })
        }
    }
}

#[cfg(not(unix))]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.trim() == std::process::id().to_string() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
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
    fn a_live_holder_excludes_another_acquisition() {
        let dir = temp_dir("live");
        let first = InstanceLock::acquire(&dir).unwrap();
        assert!(InstanceLock::acquire(&dir).is_err());
        drop(first);
        assert!(InstanceLock::acquire(&dir).is_ok());
    }

    #[test]
    fn stale_pid_text_does_not_block_startup() {
        let dir = temp_dir("stale");
        std::fs::write(dir.join("xspiderd.lock"), "4194303999\n").unwrap();
        let _lock = InstanceLock::acquire(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("xspiderd.lock"))
                .unwrap()
                .trim(),
            std::process::id().to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn releasing_preserves_the_inode_for_future_holders() {
        use std::os::unix::fs::MetadataExt;
        let dir = temp_dir("inode");
        let path = dir.join("xspiderd.lock");
        let first = InstanceLock::acquire(&dir).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        drop(first);
        let _second = InstanceLock::acquire(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    }
}

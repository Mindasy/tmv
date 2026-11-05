//! 目标进程生命周期监听。
//!
//! 用 `kill(pid, 0)` 做存在性探测：一次系统调用即可，不需要读取 `/proc`。
//! 一旦判定退出就锁存结论，避免对已消失的 pid 反复采样。

/// 目标进程存活检查。
#[derive(Debug, Clone)]
pub struct ProcessWatch {
    pid: i32,
    alive: bool,
}

impl ProcessWatch {
    pub fn new(pid: i32) -> Self {
        Self {
            pid,
            alive: probe(pid),
        }
    }

    /// 目标进程是否仍在运行；判定退出后不再重新探测。
    pub fn is_alive(&mut self) -> bool {
        if !self.alive {
            return false;
        }
        self.alive = probe(self.pid);
        self.alive
    }

    /// 不重复探测，只读取当前结论。
    pub fn alive(&self) -> bool {
        self.alive
    }
}

#[cfg(unix)]
fn probe(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 只做存在性与权限检查，不会给目标发送信号。
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM 表示进程存在但当前用户无权发信号，仍算存活。
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn probe(pid: i32) -> bool {
    pid > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_pid_is_dead() {
        let mut watch = ProcessWatch::new(2_000_000_000);
        assert!(!watch.is_alive());
        assert!(!watch.alive());
    }

    #[cfg(unix)]
    #[test]
    fn running_child_is_alive_until_reaped() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("启动 sleep");
        let mut watch = ProcessWatch::new(child.id() as i32);
        assert!(watch.is_alive());

        let _ = child.kill();
        let _ = child.wait();
        assert!(!watch.is_alive());
    }
}

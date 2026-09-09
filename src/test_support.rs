//! 测试辅助：共享的 `BILISUM_HOME` 隔离。
//!
//! 多个模块的测试都会改写进程级环境变量 `BILISUM_HOME`，必须用**同一把锁**串行化，
//! 否则并行测试会互相踩（每个模块各自加锁无法防止跨模块竞争）。

#![cfg(test)]

use std::sync::{Mutex, MutexGuard, OnceLock};

/// 全局测试锁。
pub fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

/// 在隔离的临时 `BILISUM_HOME` 下运行 `f`。
pub fn with_temp_home<T>(prefix: &str, f: impl FnOnce() -> T) -> T {
    let dir = std::env::temp_dir().join(format!("bilisum-{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    let guard = lock();
    std::env::set_var("BILISUM_HOME", &dir);
    let result = f();
    std::env::remove_var("BILISUM_HOME");
    drop(guard);
    std::fs::remove_dir_all(&dir).ok();
    result
}

//! 音频缓存 LRU 淘汰的端到端验证。

/// 超过上限时按修改时间淘汰最旧，保留最新。
#[test]
fn lru_evicts_oldest_and_keeps_newest() {
    let dir = std::env::temp_dir().join(format!("bilisum-lru-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    // 隔离 HOME；同步测试，锁不跨 await
    let _guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());
    std::env::set_var("BILISUM_HOME", &dir);
    bilisum::paths::ensure_layout().unwrap();

    std::fs::write(bilisum::paths::audio_cache_file("OLD").unwrap(), vec![0u8; 1000]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(bilisum::paths::audio_cache_file("NEW").unwrap(), vec![0u8; 1000]).unwrap();

    assert_eq!(bilisum::cache::total_size().unwrap(), 2000);

    let removed = bilisum::cache::enforce_limit(1000).unwrap();
    assert_eq!(removed, 1, "应淘汰 1 个文件");
    assert!(
        bilisum::cache::exists("NEW").unwrap().is_some(),
        "最新文件应保留"
    );
    assert!(
        bilisum::cache::exists("OLD").unwrap().is_none(),
        "最旧文件应被淘汰"
    );
    assert_eq!(bilisum::cache::total_size().unwrap(), 1000);

    std::env::remove_var("BILISUM_HOME");
    drop(_guard);
    std::fs::remove_dir_all(&dir).ok();
}

/// 未超上限时不做任何删除。
#[test]
fn lru_noop_when_under_limit() {
    let dir = std::env::temp_dir().join(format!("bilisum-lru2-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let _guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());
    std::env::set_var("BILISUM_HOME", &dir);
    bilisum::paths::ensure_layout().unwrap();
    std::fs::write(bilisum::paths::audio_cache_file("KEEP").unwrap(), vec![0u8; 100]).unwrap();

    assert_eq!(bilisum::cache::enforce_limit(10_000).unwrap(), 0);
    assert!(bilisum::cache::exists("KEEP").unwrap().is_some());

    std::env::remove_var("BILISUM_HOME");
    drop(_guard);
    std::fs::remove_dir_all(&dir).ok();
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

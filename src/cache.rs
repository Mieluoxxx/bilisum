//! 音频缓存：全局 `~/.bili/cache/{video_key}.wav` + LRU 淘汰。
//!
//! session 通过 `meta.json.audio_cache_key` 软引用缓存（不建符号链接）。

use std::path::PathBuf;

use crate::error::{AppError, IntoAppResult, Result};
use crate::paths;

/// 缓存条目。
pub struct CacheEntry {
    pub path: PathBuf,
    pub key: String,
    pub size: u64,
    pub modified: std::time::SystemTime,
}

/// 列出全部缓存条目（按修改时间倒序，最新在前）。
pub fn list() -> Result<Vec<CacheEntry>> {
    let dir = paths::cache_dir().map_err(|err| AppError::system(err.to_string()))?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut items: Vec<CacheEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("wav") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            let key = path.file_stem()?.to_string_lossy().to_string();
            Some(CacheEntry {
                path,
                key,
                size: meta.len(),
                modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
            })
        })
        .collect();
    items.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
    Ok(items)
}

/// 缓存总占用（字节）。
pub fn total_size() -> Result<u64> {
    Ok(list()?.iter().map(|entry| entry.size).sum())
}

/// 清空缓存，返回删除的文件数与释放的字节数。
pub fn clean() -> Result<(usize, u64)> {
    let entries = list()?;
    let mut removed = 0;
    let mut freed = 0;
    for entry in entries {
        if std::fs::remove_file(&entry.path).is_ok() {
            removed += 1;
            freed += entry.size;
        }
    }
    Ok((removed, freed))
}

/// 按上限淘汰最旧的条目，返回淘汰的文件数。
pub fn enforce_limit(max_bytes: u64) -> Result<usize> {
    let mut entries = list()?;
    let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
    if total <= max_bytes {
        return Ok(0);
    }
    // list() 已按最新在前排序：从尾部（最旧）开始删
    let mut removed = 0;
    while total > max_bytes && !entries.is_empty() {
        let oldest = entries.pop().expect("非空");
        if std::fs::remove_file(&oldest.path).is_ok() {
            total = total.saturating_sub(oldest.size);
            removed += 1;
        }
    }
    Ok(removed)
}

/// 缓存是否命中。
pub fn exists(video_key: &str) -> Result<Option<PathBuf>> {
    let path = paths::audio_cache_file(video_key).map_err(|err| AppError::system(err.to_string()))?;
    Ok(if path.is_file() { Some(path) } else { None })
}

/// 删除单个缓存文件（`--no-cache` 时先清）。
pub fn remove(video_key: &str) -> Result<bool> {
    let path = paths::audio_cache_file(video_key).map_err(|err| AppError::system(err.to_string()))?;
    if !path.is_file() {
        return Ok(false);
    }
    std::fs::remove_file(&path).sys_context(format!("删除缓存失败: {}", path.display()))?;
    Ok(true)
}

/// 缓存文件的修改时间（用于 `cache ls` 展示）。
pub fn modified_label(entry: &CacheEntry) -> String {
    let secs = entry
        .modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::types::format_local_time(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        crate::test_support::with_temp_home("cache", f)
    }

    fn write_cache(key: &str, bytes: usize) {
        paths::ensure_layout().unwrap();
        let path = paths::audio_cache_file(key).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn list_reports_wav_only() {
        with_temp_home(|| {
            write_cache("BV1", 100);
            std::fs::write(paths::cache_dir().unwrap().join("notes.txt"), "x").unwrap();
            let entries = list().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, "BV1");
        });
    }

    #[test]
    fn enforce_limit_evicts_oldest_first() {
        with_temp_home(|| {
            write_cache("old", 1000);
            // 让 old 的 mtime 更早
            std::thread::sleep(std::time::Duration::from_millis(20));
            write_cache("new", 1000);
            assert_eq!(total_size().unwrap(), 2000);
            let removed = enforce_limit(1000).unwrap();
            assert_eq!(removed, 1);
            assert!(exists("new").unwrap().is_some(), "最新应保留");
            assert!(exists("old").unwrap().is_none(), "最旧应淘汰");
        });
    }

    #[test]
    fn enforce_limit_noop_when_under_limit() {
        with_temp_home(|| {
            write_cache("a", 100);
            assert_eq!(enforce_limit(10_000).unwrap(), 0);
            assert!(exists("a").unwrap().is_some());
        });
    }

    #[test]
    fn clean_removes_everything() {
        with_temp_home(|| {
            write_cache("a", 10);
            write_cache("b", 20);
            let (removed, freed) = clean().unwrap();
            assert_eq!(removed, 2);
            assert_eq!(freed, 30);
            assert!(list().unwrap().is_empty());
        });
    }

    #[test]
    fn remove_single_key() {
        with_temp_home(|| {
            write_cache("k", 10);
            assert!(remove("k").unwrap());
            assert!(!remove("k").unwrap());
        });
    }
}

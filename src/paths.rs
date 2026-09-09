//! `~/.bili` 目录布局解析。所有路径集中在此，便于测试与迁移。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 应用数据根目录（默认 `~/.bili`，可用 `BILISUM_HOME` 覆盖以便测试）。
pub fn home() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("BILISUM_HOME") {
        if !custom.trim().is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    let base = dirs::home_dir().context("无法定位用户主目录")?;
    Ok(base.join(".bili"))
}

/// 确保根目录及全部子目录存在。
pub fn ensure_layout() -> Result<PathBuf> {
    let home = home()?;
    for dir in [
        home.clone(),
        sessions_dir()?,
        cache_dir()?,
        models_dir()?,
        logs_dir()?,
    ] {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("创建目录失败: {}", dir.display()))?;
    }
    Ok(home)
}

pub fn config_file() -> Result<PathBuf> {
    Ok(home()?.join("config.toml"))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(home()?.join("sessions"))
}

pub fn session_dir(id: &str) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(id))
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(home()?.join("cache"))
}

pub fn models_dir() -> Result<PathBuf> {
    Ok(home()?.join("models"))
}

pub fn model_file(name: &str) -> Result<PathBuf> {
    Ok(models_dir()?.join(name))
}

pub fn logs_dir() -> Result<PathBuf> {
    Ok(home()?.join("logs"))
}

/// B 站 cookie 文件（`bilisum login` 写入）。
pub fn bilibili_cookie_file() -> Result<PathBuf> {
    Ok(home()?.join("cookies-bilibili.txt"))
}

/// YouTube cookie 文件（用户自备，login 不覆盖）。
pub fn youtube_cookie_file() -> Result<PathBuf> {
    Ok(home()?.join("cookies-youtube.txt"))
}

/// 音频缓存路径：`cache/{video_key}.wav`。
pub fn audio_cache_file(video_key: &str) -> Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{video_key}.wav")))
}

/// 路径展开 `~` 前缀（供 config 中的自定义 cookie 路径使用）。
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(base) = dirs::home_dir() {
            return base.join(rest);
        }
    }
    PathBuf::from(path)
}

/// 目录占用总字节数（递归；读取失败的文件按 0 计）。
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// 人类可读体积（如 `1.2 GB`）。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
    }

    #[test]
    fn expand_tilde_replaces_home_prefix() {
        let expanded = expand_tilde("~/x/y");
        assert!(!expanded.to_string_lossy().starts_with('~'));
        assert!(expanded.ends_with("x/y"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }
}

//! 平台识别与视频 ID 解析。

use crate::error::{AppError, Result};
use crate::types::Platform;

/// 从 URL 识别平台。
pub fn detect(url: &str) -> Result<Platform> {
    let host = host_of(url)?;
    if host.contains("bilibili.com") || host.contains("b23.tv") {
        return Ok(Platform::Bilibili);
    }
    if host.contains("youtube.com") || host.contains("youtu.be") {
        return Ok(Platform::Youtube);
    }
    Err(AppError::user("暂不支持该链接，请输入 B 站或 YouTube 视频链接"))
}

/// 提取 host（小写）。非法 URL 报用户错误。
pub fn host_of(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|_| AppError::user(format!("无效的链接: {url}")))?;
    parsed
        .host_str()
        .map(|host| host.to_ascii_lowercase())
        .ok_or_else(|| AppError::user(format!("链接缺少主机名: {url}")))
}

/// 提取 B 站 BV 号；不是 B 站链接返回 None。
pub fn bilibili_id(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    parsed
        .path_segments()?
        .find(|segment| segment.starts_with("BV"))
        .map(|segment| segment.to_string())
}

/// 提取 YouTube 视频 ID；不是 YouTube 链接返回 None。
pub fn youtube_id(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host.contains("youtu.be") {
        return parsed
            .path_segments()?
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.to_string());
    }
    let segments: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.first().copied() {
        Some("watch") => parsed
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.to_string()),
        Some("shorts") | Some("embed") | Some("live") => {
            segments.get(1).map(|segment| segment.to_string())
        }
        _ => None,
    }
}

/// 视频标识：B 站用 BV 号，YouTube 用 `youtube-{id}`，否则 `audio-{秒}`。
///
/// 同时作为 session 目录后缀与音频缓存键。
pub fn video_key(url: &str) -> String {
    if let Some(bvid) = bilibili_id(url) {
        return bvid;
    }
    if let Some(id) = youtube_id(url) {
        return format!("youtube-{id}");
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("audio-{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bilibili_variants() {
        for url in [
            "https://www.bilibili.com/video/BV1xx411c7mD",
            "https://b23.tv/abcdef",
            "https://m.bilibili.com/video/BV1xx411c7mD?p=1",
        ] {
            assert_eq!(detect(url).unwrap(), Platform::Bilibili, "{url}");
        }
    }

    #[test]
    fn detects_youtube_variants() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(detect(url).unwrap(), Platform::Youtube, "{url}");
        }
    }

    #[test]
    fn unsupported_and_invalid_urls_are_user_errors() {
        let err = detect("https://vimeo.com/123").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
        let err = detect("not a url").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }

    #[test]
    fn parses_video_ids() {
        assert_eq!(
            bilibili_id("https://www.bilibili.com/video/BV1xx411c7mD").as_deref(),
            Some("BV1xx411c7mD")
        );
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            youtube_id("https://youtu.be/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(youtube_id("https://www.youtube.com/").as_deref(), None);
        assert_eq!(bilibili_id("https://www.youtube.com/watch?v=x"), None);
    }

    #[test]
    fn video_key_uses_platform_specific_names() {
        assert_eq!(
            video_key("https://www.bilibili.com/video/BV1xx411c7mD"),
            "BV1xx411c7mD"
        );
        assert_eq!(
            video_key("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "youtube-dQw4w9WgXcQ"
        );
        assert!(video_key("https://vimeo.com/1").starts_with("audio-"));
    }
}

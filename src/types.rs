//! 核心数据类型：字幕分段、转录结果、session 元数据。

use serde::{Deserialize, Serialize};

/// 平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Bilibili,
    Youtube,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Bilibili => "bilibili",
            Platform::Youtube => "youtube",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Bilibili => "B站",
            Platform::Youtube => "YouTube",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bilibili" => Some(Platform::Bilibili),
            "youtube" => Some(Platform::Youtube),
            _ => None,
        }
    }
}

/// 字幕来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptSource {
    /// 网站字幕。
    Subtitle,
    /// 本地 Whisper 转写。
    Whisper,
}

impl TranscriptSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptSource::Subtitle => "subtitle",
            TranscriptSource::Whisper => "whisper",
        }
    }

    /// 中文标注（产物「字幕来源」段）。
    pub fn label(self) -> &'static str {
        match self {
            TranscriptSource::Subtitle => "官方字幕",
            TranscriptSource::Whisper => "Whisper 转录",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "subtitle" => Some(TranscriptSource::Subtitle),
            "whisper" => Some(TranscriptSource::Whisper),
            _ => None,
        }
    }
}

/// 单个字幕分段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self {
            start,
            end,
            text: text.into(),
        }
    }
}

/// 转录结果：纯文本 + 分段 + 来源。
#[derive(Debug, Clone)]
pub struct Transcript {
    /// 纯文本（无时间戳，每段一行）——写入 `raw.txt`。
    pub text: String,
    /// 分段结构——写入 `segments.json`。
    pub segments: Vec<Segment>,
    pub source: TranscriptSource,
}

impl Transcript {
    /// 由分段构造：文本为每段一行纯文本（无时间戳）。
    pub fn from_segments(segments: Vec<Segment>, source: TranscriptSource) -> Self {
        let text = plain_text(&segments);
        Self {
            text,
            segments,
            source,
        }
    }
}

/// 分段 → 纯文本（每段一行，无时间戳）。
pub fn plain_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 秒 → `mm:ss`（四舍五入）。
pub fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 分段 → 带时间戳文本（`[mm:ss-mm:ss] text`，段落间空行）。
pub fn timestamp_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| {
            format!(
                "[{}-{}] {}",
                format_timestamp(segment.start),
                format_timestamp(segment.end),
                segment.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `segments.json` 的落盘结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentsFile {
    pub segments: Vec<Segment>,
    pub source: TranscriptSource,
}

/// session 元数据（`meta.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub url: String,
    pub title: String,
    pub platform: Platform,
    pub source: TranscriptSource,
    /// 本次执行的模式（最后一次为准）。
    pub mode: String,
    /// 调用的模型（未调用 LLM 时为空）。
    #[serde(default)]
    pub model: String,
    /// Unix 秒。
    pub created_at: u64,
    /// 音频缓存键（命中全局缓存时记录，供 `cache clean` 与追溯）。
    #[serde(default)]
    pub audio_cache_key: String,
}

impl SessionMeta {
    pub fn created_label(&self) -> String {
        // 不引入 chrono：直接用本地时间格式化（macOS 用 date 命令，纯 std 无时区支持）
        format_local_time(self.created_at)
    }
}

/// Unix 秒 → `YYYY-MM-DD HH:MM:SS`（本地时区；失败回退 UTC 秒数）。
pub fn format_local_time(unix_secs: u64) -> String {
    // 用 `date` 命令做本地时区转换（macOS/Linux 均有），失败回退原始秒数。
    let output = std::process::Command::new("date")
        .args(["-r", &unix_secs.to_string(), "+%Y-%m-%d %H:%M:%S"])
        .output()
        .or_else(|_| {
            std::process::Command::new("date")
                .args(["-d", &format!("@{unix_secs}"), "+%Y-%m-%d %H:%M:%S"])
                .output()
        });
    match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => format!("{unix_secs}"),
    }
}

/// session 目录 id：直接使用视频标识（B站为 BV 号，YouTube 为 `youtube-{id}`）。
pub fn session_id(video_key: &str) -> String {
    video_key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment::new(start, end, text)
    }

    #[test]
    fn plain_text_is_one_line_per_segment() {
        let segments = vec![seg(0.0, 1.0, "你好"), seg(1.0, 2.0, "世界")];
        assert_eq!(plain_text(&segments), "你好\n世界");
    }

    #[test]
    fn plain_text_skips_blank_segments() {
        let segments = vec![seg(0.0, 1.0, "  "), seg(1.0, 2.0, "有内容")];
        assert_eq!(plain_text(&segments), "有内容");
    }

    #[test]
    fn timestamp_formatting_rounds_and_pads() {
        assert_eq!(format_timestamp(0.0), "00:00");
        assert_eq!(format_timestamp(59.4), "00:59");
        assert_eq!(format_timestamp(59.6), "01:00");
        assert_eq!(format_timestamp(125.0), "02:05");
    }

    #[test]
    fn timestamp_text_has_no_missing_fields() {
        let segments = vec![seg(0.0, 5.0, "a"), seg(5.0, 65.0, "b")];
        let text = timestamp_text(&segments);
        assert_eq!(text, "[00:00-00:05] a\n\n[00:05-01:05] b");
    }

    #[test]
    fn transcript_from_segments_keeps_both_views() {
        let transcript =
            Transcript::from_segments(vec![seg(0.0, 1.0, "x")], TranscriptSource::Whisper);
        assert_eq!(transcript.text, "x");
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.source, TranscriptSource::Whisper);
    }

    #[test]
    fn source_and_platform_roundtrip() {
        for platform in [Platform::Bilibili, Platform::Youtube] {
            assert_eq!(Platform::parse(platform.as_str()), Some(platform));
        }
        for source in [TranscriptSource::Subtitle, TranscriptSource::Whisper] {
            assert_eq!(TranscriptSource::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn session_id_embeds_video_key() {
        let id = session_id("BV1xx411c7mD");
        assert_eq!(id, "BV1xx411c7mD");
        assert_eq!(session_id("youtube-dQw4w9WgXcQ"), "youtube-dQw4w9WgXcQ");
    }

    #[test]
    fn local_time_formats_or_falls_back() {
        // 2024-01-01 00:00:00 UTC 附近的任意值：只要能产出非空字符串即可（时区无关）
        let text = format_local_time(1_704_067_200);
        assert!(!text.is_empty());
    }
}

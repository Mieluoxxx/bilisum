//! 产物渲染：Markdown 组装与时间戳渲染。

use crate::mode::Mode;
use crate::types::{timestamp_text, Segment, TranscriptSource};

/// 组装输出 Markdown。
pub fn build_markdown(input: MarkdownInput<'_>) -> String {
    let body = strip_markdown_title(input.summary);
    let mut markdown = format!(
        "# {}\n\n{}\n\n## 视频信息\n\n- 视频地址: {}\n- 生成时间: {}\n- 模式: {}\n- 字幕来源: {}",
        input.title,
        body,
        input.url,
        input.time,
        input.mode.as_str(),
        input.transcript_source.label()
    );
    if input.mode == Mode::Timestamp {
        // 正文已是合并后的时间戳字幕，不重复输出
        markdown.push('\n');
    }
    markdown
}

/// `build_markdown` 入参。
pub struct MarkdownInput<'a> {
    pub mode: Mode,
    pub title: &'a str,
    pub summary: &'a str,
    pub url: &'a str,
    pub time: &'a str,
    pub transcript_source: TranscriptSource,
}

/// 剥离 Markdown 首行 `# 标题` 及其后空行（LLM 常自行加标题，避免重复）。
pub fn strip_markdown_title(markdown: &str) -> String {
    let mut lines = markdown.lines().peekable();
    let mut result: Vec<&str> = Vec::new();
    let mut skipped = false;
    while let Some(line) = lines.next() {
        if !skipped && line.trim_start().starts_with("# ") {
            skipped = true;
            if let Some(next) = lines.peek() {
                if !next.trim().is_empty() {
                    result.push(lines.next().expect("已 peek"));
                } else {
                    lines.next();
                }
            }
            continue;
        }
        result.push(line);
    }
    result.join("\n").trim().to_string()
}

/// 时间戳模式产物：`[mm:ss-mm:ss] text`，段落间空行。
pub fn render_timestamp(segments: &[Segment]) -> String {
    timestamp_text(segments)
}

/// 合并相邻分段直到累计时长达到阈值（15s）。
pub fn merge_segments(segments: &[Segment], target_duration_secs: f64) -> Vec<Segment> {
    if segments.is_empty() {
        return Vec::new();
    }
    let mut chunks: Vec<Segment> = Vec::new();
    let mut current = segments[0].clone();
    for next in &segments[1..] {
        if next.end - current.start < target_duration_secs {
            current.text = format!("{} {}", current.text.trim(), next.text.trim());
            current.end = next.end;
        } else {
            chunks.push(current);
            current = next.clone();
        }
    }
    chunks.push(current);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment::new(start, end, text)
    }

    #[test]
    fn strips_leading_h1() {
        assert_eq!(strip_markdown_title("# 标题\n\n正文"), "正文");
        assert_eq!(strip_markdown_title("# 标题\n正文"), "正文");
        assert_eq!(strip_markdown_title("正文"), "正文");
    }

    #[test]
    fn markdown_includes_metadata_block() {
        let markdown = build_markdown(MarkdownInput {
            mode: Mode::Summary,
            title: "标题",
            summary: "内容",
            url: "https://x",
            time: "2026-08-11 15:30:12",
            transcript_source: TranscriptSource::Whisper,
        });
        assert!(markdown.starts_with("# 标题\n\n内容"));
        assert!(markdown.contains("- 视频地址: https://x"));
        assert!(markdown.contains("- 模式: summary"));
        assert!(markdown.contains("- 字幕来源: Whisper 转录"));
    }

    #[test]
    fn merge_concatenates_until_threshold_reached() {
        // A(0-3) B(3-7) C(7-12) D(12-18)：18-0 ≥ 15 → ABC 成块，D 独立
        // 与旧 Rust 实现（backend/src/services.rs::merge_transcript_segments）语义一致
        let input = vec![
            seg(0.0, 3.0, "A"),
            seg(3.0, 7.0, "B"),
            seg(7.0, 12.0, "C"),
            seg(12.0, 18.0, "D"),
        ];
        let merged = merge_segments(&input, 15.0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "A B C");
        assert_eq!(merged[0].start, 0.0);
        assert_eq!(merged[0].end, 12.0);
        assert_eq!(merged[1].text, "D");
        assert_eq!(merged[1].start, 12.0);
    }

    #[test]
    fn merge_last_chunk_can_be_short() {
        // A(0-5) B(5-10) C(10-15) D(15-17)：15-0 ≥ 15 → AB 成块；17-10=7 < 15 → CD 成块
        let input = vec![
            seg(0.0, 5.0, "A"),
            seg(5.0, 10.0, "B"),
            seg(10.0, 15.0, "C"),
            seg(15.0, 17.0, "D"),
        ];
        let merged = merge_segments(&input, 15.0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "A B");
        assert_eq!(merged[1].text, "C D");
    }

    #[test]
    fn merge_splits_when_threshold_exceeded() {
        let input = vec![seg(0.0, 10.0, "A"), seg(20.0, 30.0, "B")];
        let merged = merge_segments(&input, 15.0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "A");
        assert_eq!(merged[1].text, "B");
    }

    #[test]
    fn merge_empty_returns_empty() {
        assert!(merge_segments(&[], 15.0).is_empty());
    }

    #[test]
    fn merge_keeps_long_single_segment() {
        let input = vec![seg(0.0, 20.0, "长段")];
        let merged = merge_segments(&input, 15.0);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "长段");
    }

    #[test]
    fn render_timestamp_formats_each_segment() {
        let text = render_timestamp(&[seg(0.0, 5.0, "a"), seg(5.0, 65.0, "b")]);
        assert_eq!(text, "[00:00-00:05] a\n\n[00:05-01:05] b");
    }
}

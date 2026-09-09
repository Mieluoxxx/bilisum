//! 字幕抓取：B 站与 YouTube。
//!
//! 移植自 TS 核心层（`core/subtitle/`），保留关键行为：
//! - B 站必须显式声明 `Origin`，否则风控返回 403
//! - YouTube 走 `timedtext`，按语言优先级先官方字幕后自动字幕（asr）

use serde::Deserialize;

use crate::error::{AppError, Result};
use crate::http::HttpClient;
use crate::types::{Segment, Transcript, TranscriptSource};

/// B 站 API 常用请求头。
pub fn bilibili_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        ),
        ("Referer", "https://www.bilibili.com/"),
        ("Origin", "https://www.bilibili.com"),
    ]
}

#[derive(Deserialize)]
struct BiliViewResponse {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<BiliViewData>,
}

#[derive(Deserialize)]
struct BiliViewData {
    cid: Option<u64>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct BiliPlayerResponse {
    code: i64,
    data: Option<BiliPlayerData>,
}

#[derive(Deserialize)]
struct BiliPlayerData {
    subtitle: Option<BiliSubtitleGroup>,
}

#[derive(Deserialize)]
struct BiliSubtitleGroup {
    #[serde(default)]
    subtitles: Vec<BiliSubtitleItem>,
    ai_subtitle: Option<BiliSubtitleItem>,
}

#[derive(Deserialize)]
struct BiliSubtitleItem {
    subtitle_url: Option<String>,
}

#[derive(Deserialize)]
struct BiliSubtitleBody {
    #[serde(default)]
    body: Vec<BiliSubtitleLine>,
}

#[derive(Deserialize)]
struct BiliSubtitleLine {
    from: Option<f64>,
    to: Option<f64>,
    content: Option<String>,
}

/// B 站视频元信息（标题 + cid）。
pub struct BilibiliMeta {
    pub title: String,
    pub cid: u64,
}

/// 获取 B 站元信息（标题 + cid）。
pub async fn fetch_bilibili_meta(http: &HttpClient, bvid: &str) -> Result<BilibiliMeta> {
    let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
    let resp = http.get_with_headers(&url, &bilibili_headers()).await?;
    let status = resp.status;
    let body = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(AppError::system(format!(
            "获取 B 站视频信息失败（HTTP {status}）"
        )));
    }
    let payload: BiliViewResponse = serde_json::from_str(&body)
        .map_err(|err| AppError::system(format!("解析 B 站视频信息失败：{err}")))?;
    if payload.code != 0 {
        return Err(AppError::user(format!(
            "获取 B 站视频信息失败（code={}）{}",
            payload.code,
            payload.message.unwrap_or_default()
        )));
    }
    let data = payload
        .data
        .ok_or_else(|| AppError::system("B 站视频信息为空"))?;
    let cid = data
        .cid
        .ok_or_else(|| AppError::system("B 站视频缺少 CID"))?;
    Ok(BilibiliMeta {
        title: data.title.unwrap_or_else(|| "未命名视频".to_string()),
        cid,
    })
}

/// 获取 B 站字幕；无字幕返回 None。
pub async fn fetch_bilibili_subtitles(
    http: &HttpClient,
    bvid: &str,
    cid: u64,
    cookie: Option<&str>,
) -> Result<Option<Transcript>> {
    let index_url = format!("https://api.bilibili.com/x/player/v2?bvid={bvid}&cid={cid}");
    let mut headers = bilibili_headers();
    let cookie_header = cookie
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty());
    if let Some(cookie) = &cookie_header {
        headers.push(("Cookie", cookie.as_str()));
    }

    let index_resp = http.get_with_headers(&index_url, &headers).await?;
    let index_status = index_resp.status;
    let index_body = index_resp.text().await?;
    if !(200..300).contains(&index_status) {
        return Err(AppError::system(format!(
            "获取 B 站字幕索引失败（HTTP {index_status}）"
        )));
    }
    let index: BiliPlayerResponse = serde_json::from_str(&index_body)
        .map_err(|err| AppError::system(format!("解析 B 站字幕索引失败：{err}")))?;
    if index.code != 0 {
        return Err(AppError::user(format!(
            "获取 B 站字幕索引失败（code={}）",
            index.code
        )));
    }

    let group = index.data.and_then(|data| data.subtitle);
    let mut candidates = group
        .as_ref()
        .map(|group| group.subtitles.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if candidates.is_empty() {
        if let Some(ai) = group.as_ref().and_then(|group| group.ai_subtitle.as_ref()) {
            candidates.push(ai);
        }
    }
    // B站 偶发返回空字符串的 subtitle_url（实测同一次会话内会变），必须过滤
    let Some(subtitle_url) = candidates
        .iter()
        .filter_map(|item| item.subtitle_url.as_deref())
        .find(|url| !url.trim().is_empty())
    else {
        return Ok(None);
    };
    let resolved = if let Some(rest) = subtitle_url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        subtitle_url.to_string()
    };

    let body_resp = http.get_with_headers(&resolved, &headers).await?;
    let body_status = body_resp.status;
    let body_text = body_resp.text().await?;
    if !(200..300).contains(&body_status) {
        return Err(AppError::system(format!(
            "获取 B 站字幕内容失败（HTTP {body_status}）"
        )));
    }
    let body: BiliSubtitleBody = serde_json::from_str(&body_text)
        .map_err(|err| AppError::system(format!("解析 B 站字幕内容失败：{err}")))?;

    let mut segments: Vec<Segment> = body
        .body
        .into_iter()
        .filter_map(|line| {
            let text = line.content?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let start = line.from.unwrap_or(0.0);
            Some(Segment::new(start, line.to.unwrap_or(start), text))
        })
        .collect();
    if segments.is_empty() {
        return Ok(None);
    }
    segments.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Some(Transcript::from_segments(
        segments,
        TranscriptSource::Subtitle,
    )))
}

/// 获取 YouTube 标题（oEmbed）。
pub async fn fetch_youtube_title(http: &HttpClient, url: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct OEmbed {
        title: Option<String>,
    }
    let endpoint = format!(
        "https://www.youtube.com/oembed?url={}&format=json",
        urlencoding(url)
    );
    let resp = http.get(&endpoint).await?;
    let status = resp.status;
    let body = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(AppError::system(format!(
            "获取 YouTube 标题失败（HTTP {status}）"
        )));
    }
    let data: OEmbed = serde_json::from_str(&body)
        .map_err(|err| AppError::system(format!("解析 YouTube 标题失败：{err}")))?;
    Ok(data.title.unwrap_or_else(|| "未命名视频".to_string()))
}

/// 字幕语言优先级：中文 → 英文。
const SUBTITLE_LANGUAGES: [&str; 3] = ["zh-Hans", "zh", "en"];

/// 获取 YouTube 字幕；无字幕返回 None。
pub async fn fetch_youtube_subtitles(
    http: &HttpClient,
    video_id: &str,
) -> Result<Option<Transcript>> {
    for language in SUBTITLE_LANGUAGES {
        for use_asr in [false, true] {
            if let Some(segments) =
                fetch_youtube_subtitles_by_language(http, video_id, language, use_asr).await?
            {
                return Ok(Some(Transcript::from_segments(
                    segments,
                    TranscriptSource::Subtitle,
                )));
            }
        }
    }
    Ok(None)
}

async fn fetch_youtube_subtitles_by_language(
    http: &HttpClient,
    video_id: &str,
    language: &str,
    use_asr: bool,
) -> Result<Option<Vec<Segment>>> {
    let asr_suffix = if use_asr { "&kind=asr" } else { "" };
    let url = format!(
        "https://video.google.com/timedtext?lang={language}&v={video_id}&fmt=srv3{asr_suffix}"
    );
    let resp = match http.get(&url).await {
        Ok(resp) => resp,
        // 字幕探测失败按「无字幕」处理，让上层继续尝试其他语言/回退转写
        Err(_) => return Ok(None),
    };
    if !(200..300).contains(&resp.status) {
        return Ok(None);
    }
    let xml = resp.text().await?;
    let segments = parse_youtube_xml(&xml);
    Ok(if segments.is_empty() {
        None
    } else {
        Some(segments)
    })
}

/// 解析 YouTube timedtext XML（`fmt=srv3`：`<p t="毫秒" d="毫秒">text</p>`）。
pub fn parse_youtube_xml(xml: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut current: Option<(f64, f64)> = None;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(event)) if event.name().as_ref() == b"p" => {
                let mut start = 0.0;
                let mut duration = 0.0;
                for attr in event.attributes().flatten() {
                    let Ok(value) = attr.unescape_value() else {
                        continue;
                    };
                    match attr.key.as_ref() {
                        b"t" => start = value.parse::<f64>().unwrap_or(0.0) / 1000.0,
                        b"d" => duration = value.parse::<f64>().unwrap_or(0.0) / 1000.0,
                        _ => {}
                    }
                }
                current = Some((start, duration));
                text.clear();
            }
            Ok(quick_xml::events::Event::Text(event)) => {
                if current.is_some() {
                    if let Ok(value) = event.xml_content() {
                        text.push_str(&value);
                    }
                }
            }
            // `&amp;` 等实体在 quick-xml 中是独立事件，需单独解码拼接
            Ok(quick_xml::events::Event::GeneralRef(event)) => {
                if current.is_some() {
                    match event.resolve_char_ref() {
                        Ok(Some(ch)) => text.push(ch),
                        // 命名实体（amp/lt/gt/quot/apos）走这里
                        _ => {
                            let name = String::from_utf8_lossy(event.as_ref()).to_string();
                            match decode_named_entity(&name) {
                                Some(ch) => text.push(ch),
                                None => text.push_str(&format!("&{name};")),
                            }
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(event)) if event.name().as_ref() == b"p" => {
                if let Some((start, duration)) = current.take() {
                    let cleaned = text.replace(['\n', '\r'], " ").trim().to_string();
                    if !cleaned.is_empty() {
                        let end = if duration > 0.0 {
                            start + duration
                        } else {
                            start
                        };
                        segments.push(Segment::new(start, end, cleaned));
                    }
                }
                text.clear();
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    segments
}

/// XML 命名实体 → 字符（字幕里实际会出现的五种）。
fn decode_named_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

/// 最小 URL 编码（oEmbed 的 url 参数）。
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srv3_xml() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><transcript>
            <text start="0" dur="1"><p t="0" d="1000">你好</p></text>
            <text start="1" dur="1"><p t="1000" d="2000">世界</p></text>
        </transcript>"#;
        let segments = parse_youtube_xml(xml);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], Segment::new(0.0, 1.0, "你好"));
        assert_eq!(segments[1], Segment::new(1.0, 3.0, "世界"));
    }

    #[test]
    fn xml_parser_handles_entities_and_blank_lines() {
        let xml = r#"<p t="0" d="500">a &amp; b</p><p t="500" d="500">   </p>"#;
        let segments = parse_youtube_xml(xml);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "a & b");
    }

    #[test]
    fn xml_parser_returns_empty_on_garbage() {
        assert!(parse_youtube_xml("").is_empty());
        assert!(parse_youtube_xml("<html>nope</html>").is_empty());
    }

    #[test]
    fn urlencoding_escapes_reserved_chars() {
        assert_eq!(urlencoding("https://a.b/?x=1"), "https%3A%2F%2Fa.b%2F%3Fx%3D1");
    }

    #[test]
    fn skips_empty_subtitle_urls() {
        // 实测：B站 同一次会话内会返回 subtitles 数量与 url 都变化的结果，
        // 空字符串 url 必须被跳过（否则请求 "https://" → builder error）。
        let index = r#"{"code":0,"data":{"subtitle":{"subtitles":[
            {"subtitle_url":""},
            {"subtitle_url":"//aisubtitle.hdslb.com/x.json"},
            {"subtitle_url":"//aisubtitle.hdslb.com/y.json"}
        ]}}}"#;
        let parsed: BiliPlayerResponse = serde_json::from_str(index).unwrap();
        let group = parsed.data.and_then(|data| data.subtitle).unwrap();
        let found = group
            .subtitles
            .iter()
            .filter_map(|item| item.subtitle_url.as_deref())
            .find(|url| !url.trim().is_empty());
        assert_eq!(found, Some("//aisubtitle.hdslb.com/x.json"));
    }

    #[test]
    fn all_empty_subtitle_urls_yield_none() {
        let index = r#"{"code":0,"data":{"subtitle":{"subtitles":[
            {"subtitle_url":""},{"subtitle_url":null},{"subtitle_url":"   "}
        ]}}}"#;
        let parsed: BiliPlayerResponse = serde_json::from_str(index).unwrap();
        let group = parsed.data.and_then(|data| data.subtitle).unwrap();
        let found = group
            .subtitles
            .iter()
            .filter_map(|item| item.subtitle_url.as_deref())
            .find(|url| !url.trim().is_empty());
        assert!(found.is_none(), "全空应回退音频转写，而不是请求非法 URL");
    }

    #[test]
    fn bilibili_headers_declare_origin() {
        let headers = bilibili_headers();
        assert!(headers
            .iter()
            .any(|(key, value)| *key == "Origin" && *value == "https://www.bilibili.com"));
    }
}

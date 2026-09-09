//! 两阶段流水线编排。
//!
//! ```text
//! 阶段 A prepare（零 LLM 成本）
//!   detect → 标题 → 字幕抓取 / yt-dlp+whisper → raw.txt + segments.json
//! 阶段 B generate（调 LLM）
//!   polish / timestamp / summary / fulltext / custom → llm.txt
//! ```
//!
//! `app` 层不打印任何东西：只返回结果或通过回调上报进度，为将来的 TUI 留边界。

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::http::HttpClient;
use crate::mode::Mode;
use crate::platform;
use crate::prompt;
use crate::render;
use crate::session::Session;
use crate::types::{SessionMeta, Transcript, TranscriptSource};
use crate::{cache, llm, paths, subtitle, types, whisper};

/// 进度事件（阶段 + 明细）。
pub struct ProgressEvent<'a> {
    pub stage: &'a str,
    pub detail: &'a str,
}

/// 进度回调类型。
pub type ProgressFn<'a> = &'a (dyn Fn(ProgressEvent<'_>) + Send + Sync);

/// 执行选项。
pub struct RunOptions {
    /// 字幕来源：`audio`（默认）| `subtitle`。
    pub source: String,
    /// 模式。
    pub mode: Mode,
    /// 重新转写（丢弃已有 raw.txt）。
    pub force: bool,
    /// 不使用音频缓存（强制重下）。
    pub no_cache: bool,
    /// 自定义 prompt（custom 模式）。
    pub custom_prompt: String,
}

/// 阶段 A 产出。
#[derive(Debug)]
pub struct PrepareResult {
    pub session: Session,
    pub title: String,
    /// 是否复用了已有 raw.txt。
    pub reused_raw: bool,
    /// 是否命中音频缓存。
    pub reused_audio: bool,
}

/// 阶段 B 产出。
#[derive(Debug)]
pub struct GenerateResult {
    pub text: String,
    pub model: String,
}

/// 阶段 A：准备字幕。
///
/// 已有 `raw.txt` 且非 `force` 时直接复用（这正是 `-c` 的价值）。
pub async fn prepare(
    url: &str,
    options: &RunOptions,
    config: &Config,
    http: &HttpClient,
    progress: ProgressFn<'_>,
) -> Result<PrepareResult> {
    prepare_into(url, options, config, http, progress, None).await
}

/// 同 `prepare`，但可指定已有 session 目录（`-c <id> -f` 覆盖原 session 的 raw.txt）。
pub async fn prepare_into(
    url: &str,
    options: &RunOptions,
    config: &Config,
    http: &HttpClient,
    progress: ProgressFn<'_>,
    target: Option<&Session>,
) -> Result<PrepareResult> {
    let platform = platform::detect(url)?;
    let video_key = platform::video_key(url);
    let source = effective_source(&options.source, &config.stt.source);
    let cookie_header = whisper::read_cookie_header(platform);
    let cookie_file = whisper::cookie_file_for(platform);
    let session_id = target
        .map(|session| session.id.clone())
        .unwrap_or_else(|| types::session_id(&video_key));

    // 同一视频固定对应同一 session：重复执行 URL 时直接复用 raw，
    // 避免因 session id 改为 BV 号而意外覆盖原始字幕；-f 才强制重跑。
    if target.is_none() && !options.force && !options.no_cache {
        if let Ok(existing) = Session::open(&session_id) {
            if existing.has_raw() {
                tracing::info!(session = %existing.id, "复用已有视频 session");
                progress(ProgressEvent {
                    stage: "done",
                    detail: "复用已有字幕，跳过下载与转写",
                });
                return Ok(PrepareResult {
                    title: existing.meta.title.clone(),
                    session: existing,
                    reused_raw: true,
                    reused_audio: false,
                });
            }
        }
    }
    tracing::info!(
        platform = platform.as_str(),
        video_key = %video_key,
        source = %source,
        mode = options.mode.as_str(),
        force = options.force,
        no_cache = options.no_cache,
        "阶段 A 开始"
    );

    progress(ProgressEvent {
        stage: "detect",
        detail: &format!("{} · {video_key}", platform.label()),
    });

    // 标题：B 站走 view 接口，YouTube 走 oEmbed
    progress(ProgressEvent {
        stage: "fetch_title",
        detail: "获取视频标题",
    });
    let (title, bilibili_cid) = fetch_title(platform, http, url).await?;

    let mut reused_audio = false;
    let transcript = if source == "subtitle" {
        progress(ProgressEvent {
            stage: "fetch_subtitle",
            detail: "抓取网站字幕",
        });
        match fetch_subtitle(platform, http, url, bilibili_cid, cookie_header.as_deref()).await? {
            Some(transcript) => transcript,
            None => {
                // 无网站字幕 → 回退音频转写
                progress(ProgressEvent {
                    stage: "whisper",
                    detail: "无网站字幕，回退音频转写",
                });
                let (transcript, cached) =
                    transcribe(url, &video_key, cookie_file.as_deref(), options, config, progress)
                        .await?;
                reused_audio = cached;
                transcript
            }
        }
    } else {
        let (transcript, cached) =
            transcribe(url, &video_key, cookie_file.as_deref(), options, config, progress).await?;
        reused_audio = cached;
        transcript
    };

    let meta = SessionMeta {
        id: session_id,
        url: url.to_string(),
        title: title.clone(),
        platform,
        source: transcript.source,
        mode: options.mode.as_str().to_string(),
        model: String::new(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        audio_cache_key: if transcript.source == TranscriptSource::Whisper {
            video_key.clone()
        } else {
            String::new()
        },
    };
    // `-f` 语义：覆盖指定 session 的 raw.txt；新建时直接写入
    let session = Session::create(meta)?;
    session.write_transcript(&transcript, true)?;
    tracing::info!(
        session = %session.id,
        source = transcript.source.as_str(),
        segments = transcript.segments.len(),
        "阶段 A 完成，raw.txt 已落盘"
    );

    progress(ProgressEvent {
        stage: "done",
        detail: &format!("字幕已保存（{}）", transcript.source.label()),
    });

    Ok(PrepareResult {
        session,
        title,
        reused_raw: false,
        reused_audio,
    })
}

/// 复用已有 session 的字幕（`-c` 路径），不重新抓取/转写。
pub fn reuse(session: &Session) -> Result<(String, Transcript)> {
    if !session.has_raw() {
        return Err(AppError::user(format!(
            "session 没有原始字幕，无法复用: {}",
            session.id
        ))
        .with_hint("重新抓取：bilisum -c <id> -f"));
    }
    let text = session.read_raw()?;
    let segments = session.segments_or_plain()?;
    let source = session.transcript_source();
    Ok((
        session.meta.title.clone(),
        Transcript {
            text,
            segments,
            source,
        },
    ))
}

/// 阶段 B 入参。
pub struct GenerateInput<'a> {
    pub transcript: &'a Transcript,
    pub title: &'a str,
    pub url: &'a str,
    pub mode: Mode,
    pub custom_prompt: &'a str,
    pub config: &'a Config,
    pub http: &'a HttpClient,
}

/// 阶段 B：按模式生成。
pub async fn generate(
    input: GenerateInput<'_>,
    progress: ProgressFn<'_>,
) -> Result<GenerateResult> {
    let GenerateInput {
        transcript,
        title,
        url,
        mode,
        custom_prompt,
        config,
        http,
    } = input;
    if mode == Mode::Transcript {
        // 零成本逃生舱：直接输出原始字幕
        tracing::info!("transcript 模式：跳过 LLM");
        return Ok(GenerateResult {
            text: transcript.text.clone(),
            model: String::new(),
        });
    }
    if !config.has_api_key() {
        tracing::warn!(mode = mode.as_str(), "缺少 API Key，阶段 B 中止");
        return Err(AppError::missing_api_key());
    }

    progress(ProgressEvent {
        stage: "build_prompt",
        detail: "构建提示词",
    });

    let text = match mode {
        Mode::Polish => {
            // raw.txt 已经是无时间戳纯文本：整段发送，避免人为切断上下文。
            let template = prompt::resolve_prompt(mode, custom_prompt);
            let rendered = template
                .replace("{{title}}", title)
                .replace("{{transcript}}", &transcript.text);
            progress(ProgressEvent {
                stage: "llm",
                detail: "整段润色字幕",
            });
            llm::call(http, &config.llm, &rendered, None).await?
        }
        Mode::Timestamp => {
            let lines: Vec<String> = transcript
                .segments
                .iter()
                .map(|segment| segment.text.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
            let corrected = correct_lines(&lines, title, config, http, progress).await?;
            let corrected_segments: Vec<types::Segment> = transcript
                .segments
                .iter()
                .zip(corrected.iter())
                .map(|(segment, text)| types::Segment::new(segment.start, segment.end, text.clone()))
                .collect();
            if mode == Mode::Timestamp {
                let merged = render::merge_segments(&corrected_segments, 15.0);
                render::render_timestamp(&merged)
            } else {
                corrected.join("\n")
            }
        }
        Mode::Summary | Mode::Fulltext | Mode::Custom => {
            let plain = transcript
                .segments
                .iter()
                .map(|segment| segment.text.trim())
                .collect::<Vec<_>>()
                .join("\n");
            let template = prompt::resolve_prompt(mode, custom_prompt);
            let rendered = template
                .replace("{{title}}", title)
                .replace("{{transcript}}", &plain);
            progress(ProgressEvent {
                stage: "llm",
                detail: "调用大模型",
            });
            llm::call(http, &config.llm, &rendered, None).await?
        }
        Mode::Transcript => unreachable!("已在上面提前返回"),
    };

    let model = if mode.uses_llm() {
        if config.llm.model.trim().is_empty() {
            "gpt-4o-mini".to_string()
        } else {
            config.llm.model.trim().to_string()
        }
    } else {
        String::new()
    };

    progress(ProgressEvent {
        stage: "render",
        detail: "组装产物",
    });
    let final_text = match mode {
        // 字幕类模式：直接是文本产物（可被 `-o` 直接写文件）
        Mode::Polish | Mode::Timestamp | Mode::Transcript => text,
        // 摘要类模式：套 Markdown 骨架
        _ => render::build_markdown(render::MarkdownInput {
            mode,
            title,
            summary: &text,
            url,
            time: &types::format_local_time(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
            transcript_source: transcript.source,
        }),
    };

    progress(ProgressEvent {
        stage: "done",
        detail: "生成完成",
    });
    Ok(GenerateResult {
        text: final_text,
        model,
    })
}

/// timestamp 模式的分块 1:1 校对。
async fn correct_lines(
    lines: &[String],
    title: &str,
    config: &Config,
    http: &HttpClient,
    progress: ProgressFn<'_>,
) -> Result<Vec<String>> {
    use std::collections::HashMap;

    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let chunk = prompt::TIMESTAMP_CHUNK_SIZE;
    let concurrency = prompt::TIMESTAMP_BATCH_CONCURRENCY;
    let mut corrected: HashMap<usize, String> = HashMap::new();

    let mut batch_start = 0;
    while batch_start < lines.len() {
        let mut batch = Vec::new();
        for offset in 0..concurrency {
            let start = batch_start + offset * chunk;
            if start >= lines.len() {
                break;
            }
            let end = (start + chunk).min(lines.len());
            let numbered = lines[start..end]
                .iter()
                .enumerate()
                .map(|(index, text)| format!("{}. {}", start + index + 1, text))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt_text = prompt::build_timestamp_chunk_prompt(title, &numbered);
            // 不下发 max_tokens：推理型模型（如 DeepSeek reasoner）的 reasoning_content
            // 会先吃掉预算，导致 content 为空。分块本身只有 10 行，输出天然有界，
            // 由服务端默认上限约束即可。
            let llm_config = config.llm.clone();
            let http = http.clone();
            batch.push(tokio::spawn(async move {
                llm::call(&http, &llm_config, &prompt_text, None).await
            }));
        }
        if batch.is_empty() {
            break;
        }
        for handle in batch {
            let raw = handle
                .await
                .map_err(|err| AppError::system(format!("校对任务失败：{err}")))??;
            for line in raw.lines() {
                if let Some((index, text)) = parse_numbered_line(line) {
                    if index >= 1 && index <= lines.len() && !text.is_empty() {
                        corrected.insert(index - 1, text);
                    }
                }
            }
        }
        batch_start += concurrency * chunk;
        let done = batch_start.min(lines.len());
        progress(ProgressEvent {
            stage: "llm",
            detail: &format!("正在校对字幕 {done}/{} 行", lines.len()),
        });
    }

    // 缺失行回退原字幕
    Ok(lines
        .iter()
        .enumerate()
        .map(|(index, original)| {
            corrected
                .get(&index)
                .cloned()
                .unwrap_or_else(|| original.clone())
        })
        .collect())
}

/// 解析 `1. 文本` 形式的编号行。
fn parse_numbered_line(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    let (number, rest) = trimmed.split_once(['.', '、'])?;
    let index: usize = number.trim().parse().ok()?;
    Some((index, rest.trim().to_string()))
}

/// 有效字幕来源：显式参数优先，否则用配置。
fn effective_source(cli: &str, config: &str) -> String {
    if cli == "audio" || cli == "subtitle" {
        cli.to_string()
    } else if config == "audio" || config == "subtitle" {
        config.to_string()
    } else {
        "audio".to_string()
    }
}

/// 抓标题（B 站顺带取 cid）。
async fn fetch_title(
    platform: types::Platform,
    http: &HttpClient,
    url: &str,
) -> Result<(String, Option<u64>)> {
    match platform {
        types::Platform::Bilibili => {
            let bvid = platform::bilibili_id(url)
                .ok_or_else(|| AppError::user("无法从链接中解析 BV 号"))?;
            let meta = subtitle::fetch_bilibili_meta(http, &bvid).await?;
            Ok((meta.title, Some(meta.cid)))
        }
        types::Platform::Youtube => Ok((subtitle::fetch_youtube_title(http, url).await?, None)),
    }
}

/// 抓网站字幕。
async fn fetch_subtitle(
    platform: types::Platform,
    http: &HttpClient,
    url: &str,
    cid: Option<u64>,
    cookie_header: Option<&str>,
) -> Result<Option<Transcript>> {
    match platform {
        types::Platform::Bilibili => {
            let bvid = platform::bilibili_id(url)
                .ok_or_else(|| AppError::user("无法从链接中解析 BV 号"))?;
            let cid = cid.ok_or_else(|| AppError::system("缺少 CID，无法获取字幕"))?;
            subtitle::fetch_bilibili_subtitles(http, &bvid, cid, cookie_header).await
        }
        types::Platform::Youtube => {
            let video_id = platform::youtube_id(url)
                .ok_or_else(|| AppError::user("无法从链接中解析 YouTube 视频 ID"))?;
            subtitle::fetch_youtube_subtitles(http, &video_id).await
        }
    }
}

/// 音频转写（yt-dlp + whisper-rs）。返回 (转录, 是否命中缓存)。
async fn transcribe(
    url: &str,
    video_key: &str,
    cookie_file: Option<&std::path::Path>,
    options: &RunOptions,
    config: &Config,
    progress: ProgressFn<'_>,
) -> Result<(Transcript, bool)> {
    let cached_before = cache::exists(video_key)?.is_some();
    let line_progress = |stage: &str, detail: &str| {
        progress(ProgressEvent {
            stage: if stage.is_empty() { "whisper" } else { stage },
            detail,
        })
    };
    // 先校验模型再下载音频：缺模型时不必让用户白等几分钟下载
    let model_path = whisper::resolve_model_path(&config.stt.model)?;
    let wav = whisper::download_audio(
        url,
        video_key,
        cookie_file,
        options.no_cache,
        &line_progress,
    )
    .await?;
    let language = whisper::map_stt_language(&config.stt.language);
    let transcript = whisper::transcribe(wav, model_path, language, &line_progress).await?;
    Ok((transcript, cached_before && !options.no_cache))
}

/// 加载配置并确保目录布局存在。
pub fn bootstrap() -> Result<Config> {
    paths::ensure_layout().map_err(|err| AppError::system(err.to_string()))?;
    Config::load()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numbered_lines() {
        assert_eq!(parse_numbered_line("1. 你好"), Some((1, "你好".to_string())));
        assert_eq!(parse_numbered_line(" 12、世界 "), Some((12, "世界".to_string())));
        assert_eq!(parse_numbered_line("没有编号"), None);
        assert_eq!(parse_numbered_line("x. 文本"), None);
    }

    #[test]
    fn effective_source_prefers_cli_then_config() {
        assert_eq!(effective_source("subtitle", "audio"), "subtitle");
        assert_eq!(effective_source("audio", "subtitle"), "audio");
        assert_eq!(effective_source("", "subtitle"), "subtitle");
        assert_eq!(effective_source("", ""), "audio");
        assert_eq!(effective_source("bogus", "bogus"), "audio");
    }

    #[test]
    fn bootstrap_creates_layout() {
        crate::test_support::with_temp_home("boot", || {
            let config = bootstrap().unwrap();
            assert_eq!(config.default_mode(), Mode::Polish);
            let home = crate::paths::home().unwrap();
            assert!(home.join("sessions").is_dir());
            assert!(home.join("cache").is_dir());
            assert!(home.join("models").is_dir());
            assert!(home.join("logs").is_dir());
        });
    }

    #[tokio::test]
    async fn transcript_mode_needs_no_api_key() {
        let http = HttpClient::new().unwrap();
        let transcript = Transcript::from_segments(
            vec![types::Segment::new(0.0, 1.0, "你好")],
            TranscriptSource::Whisper,
        );
        let result = generate(
            GenerateInput {
                transcript: &transcript,
                title: "标题",
                url: "https://x",
                mode: Mode::Transcript,
                custom_prompt: "",
                config: &Config::default(),
                http: &http,
            },
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.text, "你好");
        assert!(result.model.is_empty());
    }

    #[tokio::test]
    async fn llm_mode_without_key_is_user_error() {
        let http = HttpClient::new().unwrap();
        let transcript = Transcript::from_segments(
            vec![types::Segment::new(0.0, 1.0, "你好")],
            TranscriptSource::Whisper,
        );
        let err = generate(
            GenerateInput {
                transcript: &transcript,
                title: "标题",
                url: "https://x",
                mode: Mode::Polish,
                custom_prompt: "",
                config: &Config::default(),
                http: &http,
            },
            &|_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }
}

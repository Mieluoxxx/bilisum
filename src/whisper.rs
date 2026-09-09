//! 音频获取与 Whisper 转写。
//!
//! 链路：yt-dlp 下载音频 → ffmpeg 转 16k 单声道 wav → whisper-rs 内嵌推理。

use std::path::{Path, PathBuf};

use crate::error::{AppError, IntoAppResult, Result};
use crate::paths;
use crate::runner;
use crate::types::{Segment, Transcript, TranscriptSource};

/// 进度回调（阶段名 + 明细）。
pub type Progress<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// `zh-cn` → whisper.cpp 的 `zh` 语言码。
pub fn map_stt_language(language: &str) -> String {
    match language {
        "zh-cn" | "zh" => "zh".to_string(),
        "en" => "en".to_string(),
        other => other.to_string(),
    }
}

/// 下载音频到全局缓存：`cache/{video_key}.wav`。
///
/// 命中缓存直接返回（除非 `no_cache`）。
pub async fn download_audio(
    url: &str,
    video_key: &str,
    cookie_file: Option<&Path>,
    no_cache: bool,
    progress: Progress<'_>,
) -> Result<PathBuf> {
    let cache_path = paths::audio_cache_file(video_key)
        .map_err(|err| AppError::system(err.to_string()))?;
    if !no_cache && cache_path.is_file() {
        tracing::info!(path = %cache_path.display(), "音频缓存命中");
        progress("whisper", &format!("复用缓存音频 {}", cache_path.display()));
        return Ok(cache_path);
    }
    paths::ensure_layout().map_err(|err| AppError::system(err.to_string()))?;
    if no_cache {
        let _ = crate::cache::remove(video_key);
    }

    // yt-dlp 直接输出 16k 单声道 wav（需 ffmpeg 做后处理）
    let tmp_dir = paths::cache_dir()
        .map_err(|err| AppError::system(err.to_string()))?
        .join(format!(".tmp-{video_key}"));
    std::fs::create_dir_all(&tmp_dir).sys_context("创建临时目录失败")?;

    let mut args: Vec<String> = vec![
        "-x".into(),
        "--audio-format".into(),
        "wav".into(),
        "--audio-quality".into(),
        "0".into(),
        "--postprocessor-args".into(),
        "-ar 16000 -ac 1".into(),
        "-o".into(),
        format!("{}/audio.%(ext)s", tmp_dir.display()),
    ];
    if let Some(cookie) = cookie_file {
        if cookie.is_file() {
            args.push("--cookies".into());
            args.push(cookie.display().to_string());
        }
    }
    args.push(url.to_string());

    let progress_line = |line: &str| progress("whisper", line);
    tracing::info!(args = ?args, "调用 yt-dlp 下载音频");
    let result = runner::run("yt-dlp", &args, progress_line).await?;
    if !result.is_ok() {
        tracing::error!(exit_code = result.exit_code, "yt-dlp 失败");
        let tail = result.stderr_tail(20);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(AppError::system(format!(
            "下载音频失败（yt-dlp 退出码 {}）：{tail}",
            result.exit_code
        ))
        .with_hint("检查网络、视频链接或 cookie：bilisum doctor"));
    }

    let downloaded = tmp_dir.join("audio.wav");
    if !downloaded.is_file() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(AppError::system("音频转换未生成预期文件")
            .with_hint("确认已安装 ffmpeg：brew install ffmpeg"));
    }
    std::fs::rename(&downloaded, &cache_path).sys_context(format!(
        "写入缓存失败: {}",
        cache_path.display()
    ))?;
    let _ = std::fs::remove_dir_all(&tmp_dir);

    // 写入新缓存后按上限淘汰最旧
    let config = crate::config::Config::load()?;
    let max_bytes = (config.cache.max_size_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
    if max_bytes > 0 {
        if let Ok(evicted) = crate::cache::enforce_limit(max_bytes) {
            if evicted > 0 {
                progress("whisper", &format!("缓存超出上限，已淘汰 {evicted} 个旧文件"));
            }
        }
    }
    Ok(cache_path)
}

/// 用 whisper-rs 转写 wav。
///
/// 阻塞式推理放在 `spawn_blocking` 中，避免占死 tokio 工作线程。
pub async fn transcribe(
    wav_path: PathBuf,
    model_path: PathBuf,
    language: String,
    progress: Progress<'_>,
) -> Result<Transcript> {
    tracing::info!(model = %model_path.display(), language = %language, "开始 whisper 转写");
    progress("whisper", &format!("加载模型 {}", model_path.display()));
    let segments = tokio::task::spawn_blocking(move || {
        transcribe_blocking(&wav_path, &model_path, &language)
    })
    .await
    .map_err(|err| AppError::system(format!("转写任务失败：{err}")))??;

    if segments.is_empty() {
        return Err(AppError::system("语音转写结果为空")
            .with_hint("确认音频包含人声，或换用更大的模型：bilisum models"));
    }
    progress("whisper", &format!("转写完成，共 {} 段", segments.len()));
    Ok(Transcript::from_segments(segments, TranscriptSource::Whisper))
}

/// 屏蔽 whisper.cpp 的逐 token 日志（默认会刷满 stderr，污染 CLI 输出）。
///
/// 只安装一次；回调本身什么都不做。
fn install_quiet_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // 安全：回调是纯 C ABI 的 no-op，不 panic、不 unwind
        unsafe extern "C" fn silent(
            _level: u32,
            _text: *const std::ffi::c_char,
            _user_data: *mut std::ffi::c_void,
        ) {
        }
        whisper_rs::set_log_callback(Some(silent), std::ptr::null_mut());
    });
}

/// 同步转写实现（whisper-rs）。
fn transcribe_blocking(
    wav_path: &Path,
    model_path: &Path,
    language: &str,
) -> Result<Vec<Segment>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    install_quiet_logging();
    let samples = read_wav_16k_mono(wav_path)?;

    let mut context_params = WhisperContextParameters::default();
    context_params.use_gpu(true);
    let ctx = WhisperContext::new_with_params(
        model_path
            .to_str()
            .ok_or_else(|| AppError::system("模型路径含非法字符"))?,
        context_params,
    )
    .map_err(|err| {
        AppError::system(format!(
            "加载 Whisper 模型失败: {}",
            model_path.display()
        ))
        .with_hint(format!("{err}\n下载模型：bilisum models"))
    })?;

    let mut state = ctx
        .create_state()
        .map_err(|err| AppError::system(format!("创建推理状态失败：{err}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // 单线程推理足够；多线程在 macOS 上收益有限且增加内存峰值
    params.set_n_threads(4);

    state
        .full(params, &samples)
        .map_err(|err| AppError::system(format!("语音转写失败：{err}")))?;

    let n_segments = state.full_n_segments();
    let mut segments = Vec::new();
    for index in 0..n_segments {
        let Some(segment) = state.get_segment(index) else {
            continue;
        };
        let text = segment
            .to_str_lossy()
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        // 时间戳单位：厘秒（centiseconds）
        let start = segment.start_timestamp() as f64 / 100.0;
        let end = segment.end_timestamp() as f64 / 100.0;
        segments.push(Segment::new(start, end.max(start), text));
    }
    Ok(segments)
}

/// 读取 wav 并转成 whisper 需要的 f32 单声道 16k 采样。
///
/// 若采样率不是 16k 或多声道，会做简单降混与线性重采样（ffmpeg 已保证 16k 单声道，
/// 此处是兜底）。
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(path)
        .map_err(|err| AppError::system(format!("读取音频失败: {}：{err}", path.display())))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|sample| sample.ok())
                .map(|sample| sample as f32 / max)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|sample| sample.ok())
            .collect(),
    };

    let mono: Vec<f32> = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    if spec.sample_rate == 16_000 {
        return Ok(mono);
    }
    Ok(resample_linear(&mono, spec.sample_rate, 16_000))
}

/// 线性重采样（兜底；质量不如 ffmpeg，但保证不会因采样率不符而崩）。
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 / ratio;
        let left = position.floor() as usize;
        let frac = (position - left as f64) as f32;
        let a = input.get(left).copied().unwrap_or(0.0);
        let b = input.get(left + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// 模型路径校验：存在且是文件。
pub fn resolve_model_path(model: &str) -> Result<PathBuf> {
    if !crate::stt_models::is_valid_id(model) {
        return Err(AppError::user(format!("无效的模型名: {model}")));
    }
    let path = paths::model_file(model).map_err(|err| AppError::system(err.to_string()))?;
    if !path.is_file() {
        return Err(AppError::user(format!("模型未下载: {model}"))
            .with_hint("下载：bilisum models"));
    }
    Ok(path)
}

/// 平台对应的 cookie 文件（不存在返回 None）。
pub fn cookie_file_for(platform: crate::types::Platform) -> Option<PathBuf> {
    let path = match platform {
        crate::types::Platform::Bilibili => paths::bilibili_cookie_file(),
        crate::types::Platform::Youtube => paths::youtube_cookie_file(),
    }
    .ok()?;
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// 读取 cookie 文件内容（B 站接口需要 Cookie 头）。
pub fn read_cookie_header(platform: crate::types::Platform) -> Option<String> {
    let path = cookie_file_for(platform)?;
    let content = std::fs::read_to_string(path).ok()?;
    // Netscape 格式：domain\tflag\tpath\tsecure\texpiry\tname\tvalue
    let mut pairs = Vec::new();
    for line in content.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 {
            pairs.push(format!("{}={}", fields[5], fields[6]));
        }
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, samples: &[f32], rate: u32, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in samples {
            writer.write_sample(*sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn reads_mono_16k_wav() {
        let dir = std::env::temp_dir().join(format!("bilisum-wav-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        write_wav(&path, &[0.0, 0.5, -0.5], 16_000, 1);
        let samples = read_wav_16k_mono(&path).unwrap();
        assert_eq!(samples.len(), 3);
        assert!((samples[1] - 0.5).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn downmixes_stereo() {
        let dir = std::env::temp_dir().join(format!("bilisum-wav-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.wav");
        write_wav(&path, &[1.0, 0.0, 0.0, 1.0], 16_000, 2);
        let samples = read_wav_16k_mono(&path).unwrap();
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.5).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resamples_48k_to_16k() {
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 / 48_000.0).sin()).collect();
        let out = resample_linear(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn resample_empty_is_empty() {
        assert!(resample_linear(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn invalid_model_name_rejected() {
        let err = resolve_model_path("../evil.bin").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }

    #[test]
    fn missing_model_offers_download_hint() {
        crate::test_support::with_temp_home("model", || {
            let err = resolve_model_path("ggml-base.bin").unwrap_err();
            assert_eq!(err.class, crate::error::ExitClass::User);
            assert!(err.hint.unwrap().contains("bilisum models"));
        });
    }
}

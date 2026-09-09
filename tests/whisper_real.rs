//! whisper-rs 真实推理验证：用 macOS `say` 生成的语音 wav 走完整转写链路。
//!
//! 需要环境变量：
//! - `BILISUM_HOME` 指向含 `models/ggml-tiny.bin` 的目录
//! - `BILISUM_TEST_WAV` 指向 16k 单声道 wav
//!
//! 未提供时跳过（避免 CI 无模型时失败）。

use bilisum::types::TranscriptSource;
use bilisum::whisper;

#[tokio::test]
async fn transcribes_real_speech_with_whisper_rs() {
    let Ok(wav) = std::env::var("BILISUM_TEST_WAV") else {
        eprintln!("跳过：未设置 BILISUM_TEST_WAV");
        return;
    };
    let Ok(home) = std::env::var("BILISUM_HOME") else {
        eprintln!("跳过：未设置 BILISUM_HOME");
        return;
    };
    let model = std::path::PathBuf::from(&home).join("models/ggml-tiny.bin");
    if !model.is_file() {
        eprintln!("跳过：模型不存在 {}", model.display());
        return;
    }

    let transcript = whisper::transcribe(
        std::path::PathBuf::from(&wav),
        model,
        "zh".to_string(),
        &|stage, detail| eprintln!("[{stage}] {detail}"),
    )
    .await
    .expect("转写应成功");

    assert_eq!(transcript.source, TranscriptSource::Whisper);
    assert!(!transcript.segments.is_empty(), "应产出至少一段");
    assert!(!transcript.text.trim().is_empty(), "文本不应为空");

    // 中文语音应包含中文字符（tiny 模型可能识别不准，但不应完全是乱码）
    let has_cjk = transcript.text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch));
    eprintln!("转写结果：{}", transcript.text);
    assert!(has_cjk, "中文语音应产出中文字符，实际: {}", transcript.text);

    // 时间戳应递增且非负
    for segment in &transcript.segments {
        assert!(segment.start >= 0.0, "时间戳不应为负");
        assert!(segment.end >= segment.start, "end 应不早于 start");
    }
}

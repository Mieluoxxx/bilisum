//! 两阶段流水线的端到端契约测试（不触网）。
//!
//! 用真实 `Session` 目录验证：阶段 A 落盘 → `-c` 复用 → 阶段 B 生成。

use bilisum::app;
use bilisum::config::Config;
use bilisum::http::HttpClient;
use bilisum::mode::Mode;
use bilisum::session::{self, Session};
use bilisum::types::{Platform, Segment, SessionMeta, Transcript, TranscriptSource};

/// 隔离的 `BILISUM_HOME`（串行，避免环境变量竞争）。
struct TempHome {
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bilisum-flow-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let guard = LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        std::env::set_var("BILISUM_HOME", &dir);
        Self { dir, _guard: guard }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        std::env::remove_var("BILISUM_HOME");
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: id.to_string(),
        url: "https://www.bilibili.com/video/BV1xx411c7mD".to_string(),
        title: "两阶段测试".to_string(),
        platform: Platform::Bilibili,
        source: TranscriptSource::Whisper,
        mode: "transcript".to_string(),
        model: String::new(),
        created_at: 1_700_000_000,
        audio_cache_key: "BV1xx411c7mD".to_string(),
    }
}

fn transcript() -> Transcript {
    Transcript::from_segments(
        vec![
            Segment::new(0.0, 5.0, "第一段字幕"),
            Segment::new(5.0, 20.0, "第二段字幕"),
        ],
        TranscriptSource::Whisper,
    )
}

#[test]
fn stage_a_writes_raw_and_segments_then_reuse_roundtrips() {
    let _home = TempHome::new("stagea");
    bilisum::paths::ensure_layout().unwrap();

    let session = Session::create(meta("BV1xx411c7mD")).unwrap();
    session.write_transcript(&transcript(), true).unwrap();

    // 阶段 A 产物齐备
    assert!(session.raw_path().is_file(), "raw.txt 应存在");
    assert!(session.segments_path().is_file(), "segments.json 应存在");
    assert_eq!(session.read_raw().unwrap(), "第一段字幕\n第二段字幕");

    // raw.txt 无时间戳（契约）
    let raw = session.read_raw().unwrap();
    assert!(!raw.contains("[00:00"), "raw.txt 不应含时间戳: {raw}");

    // `-c` 复用：拿到同样的文本与分段
    let (title, reused) = app::reuse(&session).unwrap();
    assert_eq!(title, "两阶段测试");
    assert_eq!(reused.text, raw);
    assert_eq!(reused.segments.len(), 2);
    assert_eq!(reused.source, TranscriptSource::Whisper);
}

#[test]
fn transcript_mode_needs_no_api_key_and_writes_llm_txt() {
    let _home = TempHome::new("nomode");
    bilisum::paths::ensure_layout().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let session = Session::create(meta("s-transcript")).unwrap();
    session.write_transcript(&transcript(), true).unwrap();
    let (title, transcript) = app::reuse(&session).unwrap();

    let http = HttpClient::new().unwrap();
    let config = Config::default();
    assert!(!config.has_api_key());

    // transcript 模式：零成本逃生舱
    let generated = runtime
        .block_on(app::generate(
            app::GenerateInput {
                transcript: &transcript,
                title: &title,
                url: &session.meta.url,
                mode: Mode::Transcript,
                custom_prompt: "",
                config: &config,
                http: &http,
            },
            &|_| {},
        ))
        .unwrap();
    assert_eq!(generated.text, "第一段字幕\n第二段字幕");
    assert!(generated.model.is_empty());

    session.write_llm(&generated.text).unwrap();
    assert_eq!(
        std::fs::read_to_string(session.llm_path()).unwrap(),
        generated.text
    );
}

#[test]
fn llm_mode_without_api_key_keeps_raw_intact() {
    let _home = TempHome::new("nokey");
    bilisum::paths::ensure_layout().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let session = Session::create(meta("s-nokey")).unwrap();
    session.write_transcript(&transcript(), true).unwrap();
    let (title, transcript) = app::reuse(&session).unwrap();

    let http = HttpClient::new().unwrap();
    let err = runtime
        .block_on(app::generate(
            app::GenerateInput {
                transcript: &transcript,
                title: &title,
                url: &session.meta.url,
                mode: Mode::Polish,
                custom_prompt: "",
                config: &Config::default(),
                http: &http,
            },
            &|_| {},
        ))
        .unwrap_err();

    // 用户错误（退出码 1），且 raw.txt 必须保留 —— Q36 的核心价值
    assert_eq!(err.class, bilisum::error::ExitClass::User);
    assert!(session.raw_path().is_file(), "raw.txt 必须保留");
    assert!(!session.llm_path().is_file(), "不应产生 llm.txt");
}

#[test]
fn timestamp_mode_renders_from_segments_with_merge() {
    let _home = TempHome::new("tsmode");
    bilisum::paths::ensure_layout().unwrap();

    let session = Session::create(meta("s-ts")).unwrap();
    // 0-5 + 5-20：20-0 ≥ 15 → 不合并，两块
    session.write_transcript(&transcript(), true).unwrap();
    let segments = session.segments_or_plain().unwrap();
    let merged = bilisum::render::merge_segments(&segments, 15.0);
    assert_eq!(merged.len(), 2, "20s 跨度应超过 15s 阈值，不合并");
    let rendered = bilisum::render::render_timestamp(&merged);
    assert_eq!(rendered, "[00:00-00:05] 第一段字幕\n\n[00:05-00:20] 第二段字幕");

    // 换成 0-5 + 5-12（12 < 15）→ 合并成一块，验证阈值语义
    let close = vec![
        Segment::new(0.0, 5.0, "A"),
        Segment::new(5.0, 12.0, "B"),
    ];
    let merged = bilisum::render::merge_segments(&close, 15.0);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].text, "A B");
    assert_eq!(
        bilisum::render::render_timestamp(&merged),
        "[00:00-00:12] A B"
    );
}

#[test]
fn session_id_is_readable_and_lists_newest_first() {
    let _home = TempHome::new("list");
    bilisum::paths::ensure_layout().unwrap();

    let mut old = meta("BV1");
    old.created_at = 100;
    Session::create(old).unwrap();
    let mut new = meta("BV2");
    new.created_at = 200;
    Session::create(new).unwrap();

    let ids = session::list_ids().unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "BV2", "最新应排最前");

    let latest = Session::latest().unwrap();
    assert_eq!(latest.id, "BV2");
}

#[tokio::test]
async fn missing_model_fails_before_downloading_audio() {
    // UX 回归：缺模型必须立即报错，不能让用户白等几分钟下载音频
    let _home = TempHome::new("nomodel");
    bilisum::paths::ensure_layout().unwrap();

    let http = HttpClient::new().unwrap();
    let options = bilisum::app::RunOptions {
        source: "audio".to_string(),
        mode: Mode::Transcript,
        force: false,
        no_cache: false,
        custom_prompt: String::new(),
    };
    let config = Config::default(); // stt.model = ggml-base.bin，但磁盘上没有
    let err = bilisum::app::prepare(
        "https://www.bilibili.com/video/BV1GJ411x7h7",
        &options,
        &config,
        &http,
        &|_| {},
    )
    .await
    .unwrap_err();

    assert_eq!(err.class, bilisum::error::ExitClass::User);
    assert!(err.message.contains("模型未下载"), "实际: {}", err.message);
    assert!(err.hint.unwrap_or_default().contains("bilisum models"));
    // 不应产生任何 session 目录
    assert!(bilisum::session::list_ids().unwrap().is_empty());
}

#[test]
fn effective_source_defaults_to_audio_per_config() {
    // 默认配置 source=audio（Q29：视频网站字幕质量差）
    let config = Config::default();
    assert_eq!(config.stt.source, "audio");
    // 默认模式 polish（Q28）
    assert_eq!(config.default_mode(), Mode::Polish);
}

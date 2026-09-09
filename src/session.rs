//! session 存储：目录即历史。
//!
//! ```text
//! ~/.bili/sessions/<id>/
//! ├── meta.json      # 元数据
//! ├── raw.txt        # 原始字幕（纯文本无时间戳，永不覆盖）
//! ├── segments.json  # 分段结构（timestamp 模式唯一数据源）
//! └── llm.txt        # LLM 产物（单文件覆盖）
//! ```

use std::path::{Path, PathBuf};

use crate::error::{AppError, IntoAppResult, Result};
use crate::paths;
use crate::types::{Segment, SegmentsFile, SessionMeta, Transcript, TranscriptSource};

pub const META_FILE: &str = "meta.json";
pub const RAW_FILE: &str = "raw.txt";
pub const SEGMENTS_FILE: &str = "segments.json";
pub const LLM_FILE: &str = "llm.txt";

/// 一个 session 的磁盘视图。
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub dir: PathBuf,
    pub meta: SessionMeta,
}

impl Session {
    /// 新建 session 目录（已存在时复用目录）。
    pub fn create(meta: SessionMeta) -> Result<Self> {
        let dir = paths::session_dir(&meta.id).map_err(|err| AppError::system(err.to_string()))?;
        std::fs::create_dir_all(&dir).sys_context(format!("创建 session 目录失败: {}", dir.display()))?;
        let session = Self {
            id: meta.id.clone(),
            dir,
            meta,
        };
        session.save_meta()?;
        Ok(session)
    }

    /// 打开已有 session。
    pub fn open(id: &str) -> Result<Self> {
        let dir = paths::session_dir(id).map_err(|err| AppError::system(err.to_string()))?;
        if !dir.is_dir() {
            return Err(AppError::user(format!("session 不存在: {id}"))
                .with_hint("查看全部：bilisum sessions ls"));
        }
        let meta_path = dir.join(META_FILE);
        let text = std::fs::read_to_string(&meta_path)
            .sys_context(format!("读取 session 元数据失败: {}", meta_path.display()))?;
        let meta: SessionMeta = serde_json::from_str(&text)
            .map_err(|err| AppError::system(format!("解析 session 元数据失败：{err}")))?;
        Ok(Self {
            id: id.to_string(),
            dir,
            meta,
        })
    }

    /// 最近一次 session（按 `meta.json.created_at` 倒序）。
    pub fn latest() -> Result<Self> {
        let ids = list_ids()?;
        let Some(id) = ids.first() else {
            return Err(AppError::user("还没有任何 session")
                .with_hint("先运行：bilisum <url>"));
        };
        Self::open(id)
    }

    pub fn save_meta(&self) -> Result<()> {
        let path = self.dir.join(META_FILE);
        let text = serde_json::to_string_pretty(&self.meta)
            .map_err(|err| AppError::system(format!("序列化 session 元数据失败：{err}")))?;
        std::fs::write(&path, text).sys_context(format!("写入失败: {}", path.display()))?;
        Ok(())
    }

    pub fn raw_path(&self) -> PathBuf {
        self.dir.join(RAW_FILE)
    }

    pub fn segments_path(&self) -> PathBuf {
        self.dir.join(SEGMENTS_FILE)
    }

    pub fn llm_path(&self) -> PathBuf {
        self.dir.join(LLM_FILE)
    }

    /// 是否已有原始字幕（`-c` 复用的前提）。
    pub fn has_raw(&self) -> bool {
        self.raw_path().is_file()
    }

    /// 落盘阶段 A 产物：`raw.txt` + `segments.json`。
    ///
    /// `raw.txt` 永不覆盖（除非 `force`）。
    pub fn write_transcript(&self, transcript: &Transcript, force: bool) -> Result<()> {
        let raw_path = self.raw_path();
        if !force && raw_path.is_file() {
            return Err(AppError::user(format!(
                "原始字幕已存在，未覆盖: {}",
                raw_path.display()
            ))
            .with_hint("重新转写请加 -f（--force）"));
        }
        std::fs::write(&raw_path, &transcript.text)
            .sys_context(format!("写入失败: {}", raw_path.display()))?;

        let payload = SegmentsFile {
            segments: transcript.segments.clone(),
            source: transcript.source,
        };
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|err| AppError::system(format!("序列化 segments 失败：{err}")))?;
        let segments_path = self.segments_path();
        std::fs::write(&segments_path, json)
            .sys_context(format!("写入失败: {}", segments_path.display()))?;
        // 阶段 A 被强制重跑后，旧的 LLM 产物已经不再对应新的 raw.txt。
        if force {
            let llm_path = self.llm_path();
            std::fs::remove_file(&llm_path).ok();
        }
        Ok(())
    }

    /// 读取原始字幕文本。
    pub fn read_raw(&self) -> Result<String> {
        let path = self.raw_path();
        std::fs::read_to_string(&path).sys_context(format!(
            "读取原始字幕失败: {}",
            path.display()
        ))
    }

    /// 读取分段结构。
    pub fn read_segments(&self) -> Result<SegmentsFile> {
        let path = self.segments_path();
        let text = std::fs::read_to_string(&path).sys_context(format!(
            "读取分段数据失败: {}",
            path.display()
        ))?;
        serde_json::from_str(&text)
            .map_err(|err| AppError::system(format!("解析分段数据失败：{err}")))
    }

    /// 读取分段（缺文件时回退为按行构造的无时间戳分段）。
    pub fn segments_or_plain(&self) -> Result<Vec<Segment>> {
        match self.read_segments() {
            Ok(file) => Ok(file.segments),
            Err(_) => {
                let raw = self.read_raw()?;
                Ok(raw
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| Segment::new(0.0, 0.0, line.trim()))
                    .collect())
            }
        }
    }

    /// 写入 LLM 产物（覆盖）。
    pub fn write_llm(&self, content: &str) -> Result<()> {
        let path = self.llm_path();
        std::fs::write(&path, content).sys_context(format!("写入失败: {}", path.display()))?;
        Ok(())
    }

    /// 删除 session 目录。
    pub fn remove(&self) -> Result<()> {
        std::fs::remove_dir_all(&self.dir)
            .sys_context(format!("删除失败: {}", self.dir.display()))?;
        Ok(())
    }

    /// 字幕来源（`segments.json` 缺失时回退 meta）。
    pub fn transcript_source(&self) -> TranscriptSource {
        self.read_segments()
            .map(|file| file.source)
            .unwrap_or(self.meta.source)
    }

    /// 目录内容摘要（`sessions show` 用）。
    pub fn artifacts(&self) -> Vec<(String, bool)> {
        [
            (META_FILE.to_string(), self.dir.join(META_FILE).is_file()),
            (RAW_FILE.to_string(), self.has_raw()),
            (SEGMENTS_FILE.to_string(), self.segments_path().is_file()),
            (LLM_FILE.to_string(), self.llm_path().is_file()),
        ]
        .to_vec()
    }
}

/// 列出全部 session id，按 `meta.json.created_at` 倒序（最新在前）。
pub fn list_ids() -> Result<Vec<String>> {
    let dir = paths::sessions_dir().map_err(|err| AppError::system(err.to_string()))?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut sessions: Vec<(String, u64, std::time::SystemTime)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            let modified = entry.metadata().ok()?.modified().unwrap_or(std::time::UNIX_EPOCH);
            let created_at = std::fs::read_to_string(entry.path().join(META_FILE))
                .ok()
                .and_then(|text| serde_json::from_str::<SessionMeta>(&text).ok())
                .map(|meta| meta.created_at)
                .unwrap_or(0);
            Some((id, created_at, modified))
        })
        .collect();
    sessions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
    Ok(sessions.into_iter().map(|(id, _, _)| id).collect())
}

/// 列出全部 session。
pub fn list() -> Result<Vec<Session>> {
    list_ids()?
        .iter()
        .map(|id| Session::open(id))
        .collect()
}

/// 校验 session id 安全性（防路径穿越）。
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.starts_with('.')
        && id.len() <= 200
}

/// 路径是否为 session 目录下的文件（测试辅助）。
pub fn exists_at(dir: &Path, name: &str) -> bool {
    dir.join(name).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Platform;

    /// 测试用隔离 HOME（共享锁，避免跨模块竞争）。
    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        crate::test_support::with_temp_home("session", f)
    }

    fn meta(id: &str) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            url: "https://www.bilibili.com/video/BV1xx411c7mD".to_string(),
            title: "测试视频".to_string(),
            platform: Platform::Bilibili,
            source: TranscriptSource::Whisper,
            mode: "polish".to_string(),
            model: "gpt-4o-mini".to_string(),
            created_at: 1_700_000_000,
            audio_cache_key: "BV1xx411c7mD".to_string(),
        }
    }

    fn transcript() -> Transcript {
        Transcript::from_segments(
            vec![Segment::new(0.0, 1.0, "你好"), Segment::new(1.0, 2.0, "世界")],
            TranscriptSource::Whisper,
        )
    }

    #[test]
    fn create_writes_meta_and_transcript() {
        with_temp_home(|| {
            let session = Session::create(meta("BV1xx411c7mD")).unwrap();
            session.write_transcript(&transcript(), false).unwrap();
            assert!(exists_at(&session.dir, META_FILE));
            assert!(exists_at(&session.dir, RAW_FILE));
            assert!(exists_at(&session.dir, SEGMENTS_FILE));
            assert_eq!(session.read_raw().unwrap(), "你好\n世界");
            assert_eq!(session.read_segments().unwrap().segments.len(), 2);
        });
    }

    #[test]
    fn raw_is_never_overwritten_without_force() {
        with_temp_home(|| {
            let session = Session::create(meta("s1")).unwrap();
            session.write_transcript(&transcript(), false).unwrap();
            let err = session.write_transcript(&transcript(), false).unwrap_err();
            assert_eq!(err.class, crate::error::ExitClass::User);
            // force 才允许覆盖
            session.write_transcript(&transcript(), true).unwrap();
        });
    }

    #[test]
    fn force_refresh_removes_stale_llm_output() {
        with_temp_home(|| {
            let session = Session::create(meta("BV1")).unwrap();
            session.write_transcript(&transcript(), false).unwrap();
            session.write_llm("旧的润色结果").unwrap();
            assert!(session.llm_path().is_file());

            session.write_transcript(&transcript(), true).unwrap();
            assert!(!session.llm_path().is_file());
        });
    }

    #[test]
    fn latest_returns_newest_by_created_at() {
        with_temp_home(|| {
            let mut old = meta("BV1");
            old.created_at = 100;
            Session::create(old).unwrap();
            let mut new = meta("BV2");
            new.created_at = 200;
            Session::create(new).unwrap();
            let latest = Session::latest().unwrap();
            assert_eq!(latest.id, "BV2");
        });
    }

    #[test]
    fn open_missing_session_is_user_error() {
        with_temp_home(|| {
            let err = Session::open("nope").unwrap_err();
            assert_eq!(err.class, crate::error::ExitClass::User);
            let err = Session::latest().unwrap_err();
            assert_eq!(err.class, crate::error::ExitClass::User);
        });
    }

    #[test]
    fn segments_fall_back_to_plain_lines() {
        with_temp_home(|| {
            let session = Session::create(meta("s2")).unwrap();
            session.write_transcript(&transcript(), false).unwrap();
            std::fs::remove_file(session.segments_path()).unwrap();
            let segments = session.segments_or_plain().unwrap();
            assert_eq!(segments.len(), 2);
            assert_eq!(segments[0].text, "你好");
        });
    }

    #[test]
    fn id_validation_blocks_traversal() {
        assert!(is_valid_id("BV1xx411c7mD"));
        assert!(!is_valid_id("../etc"));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id(".hidden"));
    }

    #[test]
    fn remove_deletes_directory() {
        with_temp_home(|| {
            let session = Session::create(meta("s3")).unwrap();
            session.write_transcript(&transcript(), false).unwrap();
            session.remove().unwrap();
            assert!(!session.dir.exists());
            assert!(list_ids().unwrap().is_empty());
        });
    }
}

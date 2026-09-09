//! 输出模式（Q53 纯模式，无正交开关）。
//!
//! | 模式 | 调 LLM | 说明 |
//! |---|---|---|
//! | `polish` | 是（默认） | 整段发送给大模型润色，纯文本无时间戳 |
//! | `transcript` | 否 | 直接输出 raw.txt，零成本逃生舱 |
//! | `timestamp` | 是 | 同 polish 校对 + 15s 合并 + `[mm:ss-mm:ss]` 渲染 |
//! | `summary` / `fulltext` / `custom` | 是 | 沿用现有 prompt 模板 |

use std::fmt;
use std::str::FromStr;

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Polish,
    Transcript,
    Timestamp,
    Summary,
    Fulltext,
    Custom,
}

impl Mode {
    pub const ALL: [Mode; 6] = [
        Mode::Polish,
        Mode::Transcript,
        Mode::Timestamp,
        Mode::Summary,
        Mode::Fulltext,
        Mode::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Polish => "polish",
            Mode::Transcript => "transcript",
            Mode::Timestamp => "timestamp",
            Mode::Summary => "summary",
            Mode::Fulltext => "fulltext",
            Mode::Custom => "custom",
        }
    }

    /// 中文标签（进度与结果提示用）。
    pub fn label(self) -> &'static str {
        match self {
            Mode::Polish => "润色字幕",
            Mode::Transcript => "原始字幕",
            Mode::Timestamp => "时间戳字幕",
            Mode::Summary => "摘要",
            Mode::Fulltext => "全文",
            Mode::Custom => "自定义",
        }
    }

    /// 是否需要调用大模型。
    pub fn uses_llm(self) -> bool {
        !matches!(self, Mode::Transcript)
    }

    /// 产物是否需要按 segment 粒度渲染（读 segments.json）。
    pub fn needs_segments(self) -> bool {
        matches!(self, Mode::Timestamp)
    }

    pub fn parse(value: &str) -> Result<Self> {
        value.parse()
    }
}

impl FromStr for Mode {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "polish" => Ok(Mode::Polish),
            "transcript" => Ok(Mode::Transcript),
            "timestamp" => Ok(Mode::Timestamp),
            "summary" => Ok(Mode::Summary),
            "fulltext" => Ok(Mode::Fulltext),
            "custom" => Ok(Mode::Custom),
            other => Err(AppError::user(format!("未知模式: {other}")).with_hint(format!(
                "可用：{}",
                Mode::ALL.map(Mode::as_str).join(", ")
            ))),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrips_every_mode() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.as_str()).unwrap(), mode);
        }
    }

    #[test]
    fn only_transcript_skips_llm() {
        assert!(!Mode::Transcript.uses_llm());
        for mode in Mode::ALL.iter().filter(|m| **m != Mode::Transcript) {
            assert!(mode.uses_llm(), "{mode} 应调用 LLM");
        }
    }

    #[test]
    fn only_timestamp_needs_segments() {
        assert!(Mode::Timestamp.needs_segments());
        assert!(!Mode::Polish.needs_segments());
        assert!(!Mode::Summary.needs_segments());
    }

    #[test]
    fn unknown_mode_lists_choices() {
        let err = Mode::parse("bogus").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
        assert!(err.hint.unwrap().contains("polish"));
    }
}

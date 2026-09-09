//! `~/.bili/config.toml` 读写与字段寻址。
//!
//! 字段用点分路径寻址（`llm.api_key`），供 `settings set` 与问答式设置共用同一套读写。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, IntoAppResult, Result};
use crate::paths;

/// LLM 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub api_key: String,
    /// 空 = OpenAI 官方端点。
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// 推理强度：`none`（默认，关闭推理）| `minimal` | `low` | `medium` | `high`。
    ///
    /// 推理型模型（DeepSeek reasoner 等）的 reasoning_content 会先占用输出预算，
    /// 关闭后同一次校对从 ~15s 降到 <1s。不支持该参数的端点会忽略它。
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
}

fn default_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_reasoning_effort() -> String {
    "none".to_string()
}

/// 合法的 reasoning_effort 取值。
pub const REASONING_EFFORTS: [&str; 5] = ["none", "minimal", "low", "medium", "high"];

/// 单次请求的输出上限。
///
/// 必须给足：推理型模型即使关闭 reasoning，长字幕单次调用也可能产出数千 token；
/// 之前的小额度会让 content 被截断为空（finish_reason=length）。
pub const MAX_TOKENS: u32 = 16_000;

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: String::new(),
            model: default_model(),
            reasoning_effort: default_reasoning_effort(),
        }
    }
}

/// STT（语音转写）配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    #[serde(default = "default_stt_model")]
    pub model: String,
    /// `zh-cn` | `en`。
    #[serde(default = "default_stt_language")]
    pub language: String,
    /// `audio`（默认，质量最佳）| `subtitle`（优先网站字幕，缺失回退转写）。
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_stt_model() -> String {
    "ggml-base.bin".to_string()
}

fn default_stt_language() -> String {
    "zh-cn".to_string()
}

fn default_source() -> String {
    "audio".to_string()
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            model: default_stt_model(),
            language: default_stt_language(),
            source: default_source(),
        }
    }
}

/// 输出配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// 默认模式：`polish`（默认）| `transcript` | `timestamp` | `summary` | `fulltext` | `custom`。
    #[serde(default = "default_mode")]
    pub default_mode: String,
}

fn default_mode() -> String {
    "polish".to_string()
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            default_mode: default_mode(),
        }
    }
}

/// 自定义 prompt 模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomConfig {
    #[serde(default)]
    pub prompt: String,
}

impl Default for CustomConfig {
    fn default() -> Self {
        Self {
            prompt: crate::prompt::LEGACY_PROMPT.to_string(),
        }
    }
}

/// 缓存配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// 音频缓存上限（GB），超出按 mtime 删最旧。
    #[serde(default = "default_cache_size_gb")]
    pub max_size_gb: f64,
}

fn default_cache_size_gb() -> f64 {
    5.0
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_gb: default_cache_size_gb(),
        }
    }
}

/// 完整配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub custom: CustomConfig,
    #[serde(default)]
    pub cache: CacheConfig,
}

impl Config {
    /// 读取配置；文件不存在返回默认值。
    pub fn load() -> Result<Self> {
        let path = paths::config_file().map_err(|err| AppError::system(err.to_string()))?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .sys_context(format!("读取配置失败: {}", path.display()))?;
        toml::from_str(&text).map_err(|err| {
            AppError::user(format!("配置解析失败: {}", path.display()))
                .with_hint(format!("{err}\n可删除该文件重建：rm {}", path.display()))
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_file().map_err(|err| AppError::system(err.to_string()))?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| AppError::system(format!("序列化配置失败：{err}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).sys_context("创建配置目录失败")?;
        }
        std::fs::write(path, text)
            .sys_context(format!("写入配置失败: {}", path.display()))?;
        Ok(())
    }

    /// 点分路径读取（`llm.api_key`）。
    pub fn get(&self, key: &str) -> Result<String> {
        match key {
            "llm.api_key" => Ok(self.llm.api_key.clone()),
            "llm.base_url" => Ok(self.llm.base_url.clone()),
            "llm.model" => Ok(self.llm.model.clone()),
            "llm.reasoning_effort" => Ok(self.llm.reasoning_effort.clone()),
            "stt.model" => Ok(self.stt.model.clone()),
            "stt.language" => Ok(self.stt.language.clone()),
            "stt.source" => Ok(self.stt.source.clone()),
            "output.default_mode" => Ok(self.output.default_mode.clone()),
            "custom.prompt" => Ok(self.custom.prompt.clone()),
            "cache.max_size_gb" => Ok(self.cache.max_size_gb.to_string()),
            other => Err(unknown_key(other)),
        }
    }

    /// 点分路径写入（含取值校验）。
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "llm.api_key" => self.llm.api_key = value.to_string(),
            "llm.base_url" => self.llm.base_url = value.to_string(),
            "llm.model" => self.llm.model = value.to_string(),
            "llm.reasoning_effort" => {
                if !REASONING_EFFORTS.contains(&value) {
                    return Err(AppError::user(format!("无效的推理强度: {value}"))
                        .with_hint(format!("可用：{}", REASONING_EFFORTS.join(", "))));
                }
                self.llm.reasoning_effort = value.to_string();
            }
            "stt.model" => {
                crate::stt_models::find(value).ok_or_else(|| {
                    AppError::user(format!("未知的 STT 模型: {value}"))
                        .with_hint(format!("可用：{}", crate::stt_models::ids().join(", ")))
                })?;
                self.stt.model = value.to_string();
            }
            "stt.language" => {
                if value != "zh-cn" && value != "en" {
                    return Err(AppError::user(format!(
                        "无效的语言: {value}"
                    ))
                    .with_hint("可用：zh-cn, en"));
                }
                self.stt.language = value.to_string();
            }
            "stt.source" => {
                if value != "audio" && value != "subtitle" {
                    return Err(AppError::user(format!("无效的字幕来源: {value}"))
                        .with_hint("可用：audio, subtitle"));
                }
                self.stt.source = value.to_string();
            }
            "output.default_mode" => {
                crate::mode::Mode::parse(value)?;
                self.output.default_mode = value.to_string();
            }
            "custom.prompt" => self.custom.prompt = value.to_string(),
            "cache.max_size_gb" => {
                let parsed: f64 = value.parse().map_err(|_| {
                    AppError::user(format!("无效的数值: {value}")).with_hint("示例：5")
                })?;
                if parsed < 0.0 {
                    return Err(AppError::user("缓存上限不能为负数"));
                }
                self.cache.max_size_gb = parsed;
            }
            other => return Err(unknown_key(other)),
        }
        Ok(())
    }

    /// 全部可寻址键（供 `settings` 展示）。
    pub fn keys() -> &'static [&'static str] {
        &[
            "llm.api_key",
            "llm.base_url",
            "llm.model",
            "llm.reasoning_effort",
            "stt.model",
            "stt.language",
            "stt.source",
            "output.default_mode",
            "custom.prompt",
            "cache.max_size_gb",
        ]
    }

    /// API Key 是否已配置。
    pub fn has_api_key(&self) -> bool {
        !self.llm.api_key.trim().is_empty()
    }

    /// 默认模式（配置非法时回退 polish 并记录警告）。
    pub fn default_mode(&self) -> crate::mode::Mode {
        crate::mode::Mode::parse(&self.output.default_mode).unwrap_or(crate::mode::Mode::Polish)
    }
}

fn unknown_key(key: &str) -> AppError {
    AppError::user(format!("未知配置项: {key}"))
        .with_hint(format!("可用：{}", Config::keys().join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_contract() {
        let config = Config::default();
        assert_eq!(config.output.default_mode, "polish");
        assert_eq!(config.stt.source, "audio");
        assert_eq!(config.stt.model, "ggml-base.bin");
        assert_eq!(config.llm.model, "gpt-4o-mini");
        assert_eq!(config.llm.reasoning_effort, "none");
        assert_eq!(config.cache.max_size_gb, 5.0);
        assert!(!config.has_api_key());
    }

    #[test]
    fn roundtrip_preserves_values() {
        let dir = std::env::temp_dir().join(format!("bilisum-config-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut config = Config::default();
        config.set("llm.api_key", "sk-test").unwrap();
        config.set("output.default_mode", "timestamp").unwrap();
        config.set("cache.max_size_gb", "12.5").unwrap();
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.llm.api_key, "sk-test");
        assert_eq!(loaded.default_mode(), crate::mode::Mode::Timestamp);
        assert_eq!(loaded.cache.max_size_gb, 12.5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_key_is_user_error() {
        let config = Config::default();
        let err = config.get("nope.nope").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }

    #[test]
    fn invalid_mode_rejected() {
        let mut config = Config::default();
        let err = config.set("output.default_mode", "bogus").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
        let err = config.set("stt.source", "bogus").unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }

    #[test]
    fn every_advertised_key_is_addressable() {
        let mut config = Config::default();
        for key in Config::keys() {
            assert!(config.get(key).is_ok(), "get 失败: {key}");
        }
        // 写入路径覆盖全部键（custom.prompt 用多行）
        for key in Config::keys() {
            let value = match *key {
                "llm.reasoning_effort" => "low",
                "stt.model" => "ggml-tiny.bin",
                "stt.language" => "en",
                "stt.source" => "subtitle",
                "output.default_mode" => "summary",
                "cache.max_size_gb" => "3",
                _ => "x",
            };
            assert!(config.set(key, value).is_ok(), "set 失败: {key}");
        }
    }
}

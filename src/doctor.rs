//! 环境自检（`bilisum doctor`）。
//!
//! 逐项检查运行时依赖、模型、配置与登录态，给出可执行的修复建议。

use crate::error::{AppError, Result};
use crate::http::HttpClient;
use crate::paths;
use crate::runner;
use crate::{auth, stt_models};

/// 单项检查结果。
struct Check {
    ok: bool,
    name: &'static str,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            name,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            ok: false,
            name,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    /// 非致命项（如未登录、未配 key）。
    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            ok: true,
            name,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// 运行全部检查。
pub async fn run() -> Result<()> {
    let mut checks = Vec::new();

    // 目录
    match paths::ensure_layout() {
        Ok(home) => checks.push(Check::ok("目录", format!("{}", home.display()))),
        Err(err) => checks.push(Check::fail(
            "目录",
            err.to_string(),
            format!("检查权限：{}", paths::home().map(|p| p.display().to_string()).unwrap_or_default()),
        )),
    }

    // 外部二进制
    for (program, install) in [
        ("yt-dlp", "brew install yt-dlp"),
        ("ffmpeg", "brew install ffmpeg"),
    ] {
        match runner::which(program) {
            Some(path) => checks.push(Check::ok(
                match program {
                    "yt-dlp" => "yt-dlp",
                    _ => "ffmpeg",
                },
                path.display().to_string(),
            )),
            None => checks.push(Check::fail(
                match program {
                    "yt-dlp" => "yt-dlp",
                    _ => "ffmpeg",
                },
                "未找到",
                install,
            )),
        }
    }

    // 配置
    let config = match crate::config::Config::load() {
        Ok(config) => {
            let path = paths::config_file()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            checks.push(Check::ok("配置", path));
            Some(config)
        }
        Err(err) => {
            checks.push(Check::fail("配置", err.to_string(), "bilisum settings"));
            None
        }
    };

    // STT 模型
    if let Some(config) = &config {
        let installed: Vec<&str> = stt_models::STT_MODELS
            .iter()
            .filter(|model| {
                paths::model_file(model.id)
                    .map(|path| path.is_file())
                    .unwrap_or(false)
            })
            .map(|model| model.label)
            .collect();
        if installed.is_empty() {
            checks.push(Check::fail(
                "STT 模型",
                "未下载任何模型",
                "bilisum models",
            ));
        } else if !installed.contains(&stt_models::find(&config.stt.model).map(|m| m.label).unwrap_or("")) {
            checks.push(Check::fail(
                "STT 模型",
                format!("已下载 {:?}，但配置的是 {}", installed, config.stt.model),
                format!("bilisum settings set stt.model {}", installed.first().unwrap_or(&"ggml-base.bin")),
            ));
        } else {
            checks.push(Check::ok("STT 模型", installed.join(", ")));
        }

        // API Key
        if config.has_api_key() {
            checks.push(Check::ok(
                "API Key",
                format!("已配置（模型 {}）", config.llm.model),
            ));
        } else {
            checks.push(Check::warn(
                "API Key",
                "未配置（仅能生成原始字幕）",
                "bilisum settings",
            ));
        }
    }

    // 登录态（需要网络，失败不算致命）
    let http = match HttpClient::new() {
        Ok(http) => Some(http),
        Err(err) => {
            checks.push(Check::fail("HTTP", err.to_string(), "检查网络"));
            None
        }
    };
    if let Some(http) = &http {
        let status = auth::status(http).await;
        match status {
            auth::SessionStatus::Active { .. } => checks.push(Check::ok("B 站登录", status.label())),
            auth::SessionStatus::SignedOut => {
                checks.push(Check::warn("B 站登录", "未登录", "bilisum login"))
            }
            auth::SessionStatus::Expired => {
                checks.push(Check::warn("B 站登录", "已过期", "bilisum login"))
            }
            auth::SessionStatus::ServiceError(detail) => checks.push(Check::warn(
                "B 站登录",
                format!("服务异常：{detail}"),
                "稍后重试",
            )),
        }
    }

    // 缓存占用
    if let Ok(entries) = crate::cache::list() {
        let total = entries.iter().map(|entry| entry.size).sum();
        checks.push(Check::ok(
            "音频缓存",
            format!(
                "{} 个文件，{}",
                entries.len(),
                paths::human_size(total)
            ),
        ));
    }

    // 输出
    println!("bilisum 环境自检\n");
    for check in &checks {
        let mark = if check.ok { "✓" } else { "✗" };
        println!("{mark} {:<12} {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            println!("  {:<12} → {fix}", "");
        }
    }

    let failures = checks.iter().filter(|check| !check.ok).count();
    println!();
    if failures == 0 {
        println!("全部就绪");
        Ok(())
    } else {
        Err(AppError::system(format!("{failures} 项检查未通过")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_mark_semantics() {
        assert!(Check::ok("a", "b").ok);
        assert!(!Check::fail("a", "b", "c").ok);
        assert!(Check::warn("a", "b", "c").ok);
        assert!(Check::warn("a", "b", "c").fix.is_some());
    }
}

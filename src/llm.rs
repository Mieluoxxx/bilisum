//! 大模型调用（OpenAI 兼容接口）。

use serde::Deserialize;

use crate::config::LlmConfig;
use crate::error::{AppError, Result};
use crate::http::HttpClient;

/// 端点解析结果。
pub struct Endpoint {
    pub url: String,
    pub default_model: String,
}

/// 解析端点：`base_url` 有值 → `{base}/chat/completions`，否则 OpenAI 官方。
pub fn resolve_endpoint(base_url: &str) -> Endpoint {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        Endpoint {
            url: "https://api.openai.com/v1/chat/completions".to_string(),
            default_model: "gpt-4o-mini".to_string(),
        }
    } else {
        Endpoint {
            url: format!("{trimmed}/chat/completions"),
            default_model: "gpt-4o-mini".to_string(),
        }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct Choice {
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    message: Option<String>,
}

/// 调用 LLM。
///
/// `max_tokens` 为 None 时使用 `config::MAX_TOKENS`（16k）——小额度会让推理型
/// 模型的 content 被截断为空（`finish_reason=length`）。
pub async fn call(
    http: &HttpClient,
    config: &LlmConfig,
    prompt: &str,
    max_tokens: Option<u32>,
) -> Result<String> {
    if config.api_key.trim().is_empty() {
        return Err(AppError::missing_api_key());
    }
    let endpoint = resolve_endpoint(&config.base_url);
    let model = if config.model.trim().is_empty() {
        endpoint.default_model.clone()
    } else {
        config.model.trim().to_string()
    };

    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": "你是专业视频内容总结助手" },
            { "role": "user", "content": prompt }
        ],
        "max_tokens": max_tokens.unwrap_or(crate::config::MAX_TOKENS)
    });
    // 推理型模型默认关闭推理（省时省 token）；不支持的端点会忽略该字段
    let effort = config.reasoning_effort.trim();
    if !effort.is_empty() {
        body["reasoning_effort"] = serde_json::json!(effort);
    }

    let auth = format!("Bearer {}", config.api_key.trim());
    let headers = [
        ("Content-Type", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    tracing::info!(endpoint = %endpoint.url, model = %model, max_tokens = ?max_tokens, "调用 LLM");
    let resp = http.post_json(&endpoint.url, &headers, &body).await?;
    let status = resp.status;
    let text = resp.text().await?;

    if !(200..300).contains(&status) {
        tracing::error!(status, "LLM 返回非 2xx");
        // 尽力提取服务端错误消息，便于定位（key 无效 / 模型不存在 / 余额不足）
        let detail = serde_json::from_str::<ChatResponse>(&text)
            .ok()
            .and_then(|parsed| parsed.error)
            .and_then(|err| err.message)
            .unwrap_or_else(|| text.chars().take(300).collect());
        return Err(AppError::system(format!(
            "大模型调用失败（HTTP {status}）：{detail}"
        ))
        .with_hint("检查 API Key / Base URL / 模型名：bilisum settings"));
    }

    let parsed: ChatResponse = serde_json::from_str(&text)
        .map_err(|err| AppError::system(format!("解析大模型响应失败：{err}")))?;
    let content = parsed
        .choices
        .and_then(|choices| choices.into_iter().next())
        .and_then(|choice| choice.message)
        .and_then(|message| message.content)
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty());
    content.ok_or_else(|| AppError::system("大模型未返回内容"))
}

/// 连通性测试：最小请求验证 key/base_url/model 全链路。
///
/// 不经过 `call`：`max_tokens=1` 对推理型模型会返回空 content，
/// 但 HTTP 200 + 合法响应体已足以证明链路可用，故这里只校验状态与结构。
pub async fn test_connection(http: &HttpClient, config: &LlmConfig) -> Result<String> {
    if config.api_key.trim().is_empty() {
        return Err(AppError::missing_api_key());
    }
    let endpoint = resolve_endpoint(&config.base_url);
    let model = if config.model.trim().is_empty() {
        endpoint.default_model.clone()
    } else {
        config.model.trim().to_string()
    };

    let mut body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 64
    });
    let effort = config.reasoning_effort.trim();
    if !effort.is_empty() {
        body["reasoning_effort"] = serde_json::json!(effort);
    }
    let auth = format!("Bearer {}", config.api_key.trim());
    let headers = [
        ("Content-Type", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    let resp = http.post_json(&endpoint.url, &headers, &body).await?;
    let status = resp.status;
    let text = resp.text().await?;

    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<ChatResponse>(&text)
            .ok()
            .and_then(|parsed| parsed.error)
            .and_then(|err| err.message)
            .unwrap_or_else(|| text.chars().take(300).collect());
        return Err(AppError::system(format!(
            "大模型调用失败（HTTP {status}）：{detail}"
        ))
        .with_hint("检查 API Key / Base URL / 模型名：bilisum settings"));
    }
    // 响应必须是合法 JSON 且含 choices 字段
    let parsed: ChatResponse = serde_json::from_str(&text)
        .map_err(|err| AppError::system(format!("解析响应失败：{err}")))?;
    if parsed.choices.is_none() {
        return Err(AppError::system("响应缺少 choices 字段")
            .with_hint("确认端点是否为 OpenAI 兼容接口"));
    }

    Ok(format!("连接成功（端点 {} · 模型 {model}）", endpoint.url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_openai() {
        let endpoint = resolve_endpoint("");
        assert_eq!(endpoint.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(endpoint.default_model, "gpt-4o-mini");
    }

    #[test]
    fn endpoint_uses_base_url_and_trims_slashes() {
        assert_eq!(
            resolve_endpoint("https://api.deepseek.com/v1/").url,
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("  https://x.y/v1  ").url,
            "https://x.y/v1/chat/completions"
        );
    }

    #[test]
    fn max_tokens_budget_is_large_enough_for_reasoning_models() {
        // 回归：小额度会被 reasoning_content 吃光，content 变空（finish_reason=length）
        let budget = crate::config::MAX_TOKENS;
        assert!(budget >= 16_000, "输出上限必须给推理模型留足预算，当前 {budget}");
    }

    #[test]
    fn reasoning_effort_accepts_only_documented_values() {
        assert!(crate::config::REASONING_EFFORTS.contains(&"none"));
        assert!(crate::config::REASONING_EFFORTS.contains(&"high"));
        assert!(!crate::config::REASONING_EFFORTS.contains(&"off"));
    }

    #[tokio::test]
    async fn missing_api_key_is_user_error_before_network() {
        let http = HttpClient::new().unwrap();
        let err = call(&http, &LlmConfig::default(), "x", None)
            .await
            .unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }
}

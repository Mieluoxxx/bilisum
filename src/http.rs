//! HTTP 客户端封装。
//!
//! CLI 无浏览器 CORS 限制，直接使用 `reqwest`；此层只负责统一超时、User-Agent 与响应读取。

use std::error::Error as _;
use std::time::Duration;

use crate::error::{AppError, Result};

/// 默认请求超时（秒）。字幕接口偶有慢响应，给足余量。
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// POST JSON（LLM）超时（秒）。
///
/// 60s 会被推理型模型的整段润色打断：实测 GLM-5.3 润色 5k 字字幕要 ~67s，
/// 表现为 `error sending request for url`（reqwest 的 Display 不暴露超时原因）。
const JSON_TIMEOUT_SECS: u64 = 300;

/// 简化的响应体。
pub struct HttpResponse {
    pub status: u16,
    body: String,
    /// 全部 `Set-Cookie` 头（B站登录凭证只在这里，不在 body）。
    set_cookies: Vec<String>,
}

impl HttpResponse {
    /// 响应体文本。
    pub async fn text(self) -> Result<String> {
        Ok(self.body)
    }

    /// 全部 `Set-Cookie` 头（原始形式，含属性）。
    pub fn set_cookies(&self) -> &[String] {
        &self.set_cookies
    }
}

/// HTTP 客户端。
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let inner = reqwest::Client::builder()
            .user_agent(concat!("bilisum/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|err| AppError::system(format!("初始化 HTTP 客户端失败：{err}")))?;
        Ok(Self { inner })
    }

    pub async fn get(&self, url: &str) -> Result<HttpResponse> {
        self.get_with_headers(url, &[]).await
    }

    /// GET + 自定义请求头（B 站需要 Origin/Referer/Cookie）。
    pub async fn get_with_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let mut request = self.inner.get(url);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        Self::send(request, url, DEFAULT_TIMEOUT_SECS).await
    }

    /// POST JSON。
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse> {
        let mut request = self
            .inner
            .post(url)
            .json(body)
            .timeout(Duration::from_secs(JSON_TIMEOUT_SECS));
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        Self::send(request, url, JSON_TIMEOUT_SECS).await
    }

    /// POST 表单（B 站登录接口用）。
    pub async fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<HttpResponse> {
        let mut request = self.inner.post(url).body(body.to_string());
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        Self::send(request, url, DEFAULT_TIMEOUT_SECS).await
    }

    /// 统一发送：读取状态、body 与全部 `Set-Cookie` 头。
    async fn send(
        request: reqwest::RequestBuilder,
        url: &str,
        timeout_secs: u64,
    ) -> Result<HttpResponse> {
        let resp = request.send().await.map_err(|err| {
            // reqwest 的 Display 只有 "error sending request"，超时/连接失败必须自己翻译
            let detail = if err.is_timeout() {
                format!("请求超时（{timeout_secs}s）")
            } else if err.is_connect() {
                "连接失败（检查网络或代理）".to_string()
            } else {
                err.to_string()
            };
            AppError::system(format!("请求失败 {url}：{detail}"))
        })?;
        let status = resp.status().as_u16();
        let set_cookies = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let body = resp.text().await.map_err(|err| {
            // 非流式响应要静默 60~90s 才吐出整块 body（实测 62KB），中途任何一跳
            // （代理/Cloudflare）闲太久就会掐断，表现为 "error decoding response body"
            let detail = match err.source() {
                Some(cause) => format!("{}（{cause}）", err),
                None => err.to_string(),
            };
            AppError::system(format!("读取响应失败 {url}：{detail}"))
        })?;
        Ok(HttpResponse {
            status,
            body,
            set_cookies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds() {
        assert!(HttpClient::new().is_ok());
    }
}

//! HTTP 客户端封装。
//!
//! CLI 无浏览器 CORS 限制，直接使用 `reqwest`；此层只负责统一超时、User-Agent 与响应读取。

use std::time::Duration;

use crate::error::{AppError, Result};

/// 默认请求超时（秒）。字幕接口偶有慢响应，给足余量。
const DEFAULT_TIMEOUT_SECS: u64 = 60;

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
        Self::send(request, url).await
    }

    /// POST JSON。
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse> {
        let mut request = self.inner.post(url).json(body);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        Self::send(request, url).await
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
        Self::send(request, url).await
    }

    /// 统一发送：读取状态、body 与全部 `Set-Cookie` 头。
    async fn send(request: reqwest::RequestBuilder, url: &str) -> Result<HttpResponse> {
        let resp = request
            .send()
            .await
            .map_err(|err| AppError::system(format!("请求失败 {url}：{err}")))?;
        let status = resp.status().as_u16();
        let set_cookies = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let body = resp
            .text()
            .await
            .map_err(|err| AppError::system(format!("读取响应失败 {url}：{err}")))?;
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

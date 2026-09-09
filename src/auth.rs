//! B 站扫码登录与登录态查询。
//!
//! 四态模型（对齐 GUI 版）：已登录 / 未登录 / 已过期 / 服务异常。
//! **过期不自动删除 cookie**，只在需要时提示重新登录。

use std::time::Duration;

use serde::Deserialize;

use crate::error::{AppError, IntoAppResult, Result};
use crate::http::HttpClient;
use crate::paths;
use crate::subtitle::bilibili_headers;

/// 登录态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// 已登录。
    Active { name: String, uid: u64 },
    /// 未登录（cookie 缺失或未登录）。
    SignedOut,
    /// cookie 已过期（服务端明确返回 -101）。
    Expired,
    /// 服务异常（网络/解析/其他 code）——**不视为过期**。
    ServiceError(String),
}

impl SessionStatus {
    pub fn label(&self) -> String {
        match self {
            SessionStatus::Active { name, uid } => format!("已登录：{name}（uid {uid}）"),
            SessionStatus::SignedOut => "未登录".to_string(),
            SessionStatus::Expired => "已过期".to_string(),
            SessionStatus::ServiceError(detail) => format!("服务异常：{detail}"),
        }
    }
}

#[derive(Deserialize)]
struct NavResponse {
    code: i64,
    message: Option<String>,
    data: Option<NavData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NavData {
    #[serde(default)]
    is_login: bool,
    uname: Option<String>,
    mid: Option<u64>,
}

#[derive(Deserialize)]
struct QrcodeGenerate {
    code: i64,
    message: Option<String>,
    data: Option<QrcodeData>,
}

#[derive(Deserialize)]
struct QrcodeData {
    url: Option<String>,
    qrcode_key: Option<String>,
}

#[derive(Deserialize)]
struct QrcodePoll {
    /// 外层 code 仅表示请求成功，业务状态在 data.code。
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<PollData>,
}

#[derive(Deserialize)]
struct PollData {
    /// B 站业务状态码：0=成功 / 86038=失效 / 86101=未扫 / 86090=已扫未确认。
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    url: Option<String>,
}

/// 查询登录态。
pub async fn status(http: &HttpClient) -> SessionStatus {
    let cookie = crate::whisper::read_cookie_header(crate::types::Platform::Bilibili);
    let Some(cookie) = cookie else {
        return SessionStatus::SignedOut;
    };

    let headers = [("Cookie", cookie.as_str())];
    let url = "https://api.bilibili.com/x/web-interface/nav";
    let resp = match http.get_with_headers(url, &headers).await {
        Ok(resp) => resp,
        Err(err) => return SessionStatus::ServiceError(err.message),
    };
    if !(200..300).contains(&resp.status) {
        return SessionStatus::ServiceError(format!("HTTP {}", resp.status));
    }
    let body = match resp.text().await {
        Ok(body) => body,
        Err(err) => return SessionStatus::ServiceError(err.message),
    };
    let parsed: NavResponse = match serde_json::from_str(&body) {
        Ok(parsed) => parsed,
        Err(err) => return SessionStatus::ServiceError(format!("解析失败：{err}")),
    };

    // 仅 code=-101 判定过期
    if parsed.code == -101 {
        return SessionStatus::Expired;
    }
    if parsed.code != 0 {
        return SessionStatus::ServiceError(format!(
            "code={}{}",
            parsed.code,
            parsed
                .message
                .map(|message| format!(" {message}"))
                .unwrap_or_default()
        ));
    }
    let Some(data) = parsed.data else {
        return SessionStatus::ServiceError("响应数据为空".to_string());
    };
    if !data.is_login {
        return SessionStatus::Expired;
    }
    match (data.uname, data.mid) {
        (Some(name), Some(uid)) => SessionStatus::Active { name, uid },
        _ => SessionStatus::ServiceError("响应缺少用户信息".to_string()),
    }
}

/// 打印登录态（`bilisum whoami`）。
pub async fn whoami() -> Result<()> {
    let http = HttpClient::new()?;
    let status = status(&http).await;
    println!("{}", status.label());
    if status == SessionStatus::Expired {
        println!("提示：bilisum login 重新登录");
    }
    Ok(())
}

/// 扫码登录（`bilisum login`）。
pub async fn login() -> Result<()> {
    let http = HttpClient::new()?;
    paths::ensure_layout().map_err(|err| AppError::system(err.to_string()))?;

    let payload = generate_qrcode(&http).await?;
    render_qrcode(&payload.content)?;
    println!("请用 B 站 App 扫描上方二维码（链接：{}）", payload.content);

    let cookie = poll_until_confirmed(&http, &payload.qrcode_key).await?;
    if cookie.is_empty() {
        return Err(AppError::system("登录成功但未获取到凭证"));
    }

    let path = paths::bilibili_cookie_file().map_err(|err| AppError::system(err.to_string()))?;
    std::fs::write(&path, &cookie).sys_context(format!("写入失败: {}", path.display()))?;
    println!("✓ 凭证已保存：{}", path.display());

    match status(&http).await {
        SessionStatus::Active { name, uid } => println!("✓ 已登录：{name}（uid {uid}）"),
        other => println!("! 登录态校验：{}", other.label()),
    }
    Ok(())
}

struct QrcodePayload {
    qrcode_key: String,
    content: String,
}

async fn generate_qrcode(http: &HttpClient) -> Result<QrcodePayload> {
    let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate?source=main-fe-header";
    let resp = http.get_with_headers(url, &bilibili_headers()).await?;
    if !(200..300).contains(&resp.status) {
        return Err(AppError::system(format!(
            "获取登录二维码失败（HTTP {}）",
            resp.status
        )));
    }
    let body = resp.text().await?;
    let parsed: QrcodeGenerate = serde_json::from_str(&body)
        .map_err(|err| AppError::system(format!("解析二维码响应失败：{err}")))?;
    if parsed.code != 0 {
        return Err(AppError::user(format!(
            "获取登录二维码失败（code={}）{}",
            parsed.code,
            parsed.message.unwrap_or_default()
        )));
    }
    let data = parsed
        .data
        .ok_or_else(|| AppError::system("二维码响应数据为空"))?;
    Ok(QrcodePayload {
        qrcode_key: data
            .qrcode_key
            .ok_or_else(|| AppError::system("二维码响应缺少 qrcode_key"))?,
        content: data
            .url
            .ok_or_else(|| AppError::system("二维码响应缺少 url"))?,
    })
}

/// 终端渲染二维码（qrcode crate 的 unicode 渲染，2 像素/字符）。
fn render_qrcode(content: &str) -> Result<()> {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    let code = QrCode::new(content.as_bytes())
        .map_err(|err| AppError::system(format!("生成二维码失败：{err}")))?;
    let rendered = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    println!("{rendered}");
    Ok(())
}

/// 轮询扫码状态直到确认或失效。
async fn poll_until_confirmed(http: &HttpClient, qrcode_key: &str) -> Result<String> {
    let url = format!(
        "https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key={qrcode_key}&source=main-fe-header"
    );
    let mut last_code: Option<i64> = None;
    // 二维码有效期约 180s；超时后报错
    for _ in 0..120 {
        let resp = http.get_with_headers(&url, &bilibili_headers()).await?;
        if !(200..300).contains(&resp.status) {
            return Err(AppError::system(format!(
                "查询扫码状态失败（HTTP {}）",
                resp.status
            )));
        }
        // 凭证在 Set-Cookie 头里，必须先取出再消费 body
        let set_cookies: Vec<String> = resp.set_cookies().to_vec();
        let body = resp.text().await?;
        let parsed: QrcodePoll = serde_json::from_str(&body)
            .map_err(|err| AppError::system(format!("解析扫码状态失败：{err}")))?;
        if parsed.code != 0 {
            return Err(AppError::system(format!(
                "查询扫码状态失败（code={}）{}",
                parsed.code,
                parsed.message.unwrap_or_default()
            )));
        }
        let data = parsed
            .data
            .ok_or_else(|| AppError::system("扫码状态响应为空"))?;
        let code = data.code.unwrap_or(-1);
        // 失效时带上服务端消息，便于区分风控与真的过期
        let detail = data.message.unwrap_or_default();
        if Some(code) != last_code {
            match code {
                86101 => println!("· 等待扫码…"),
                86090 => println!("· 已扫码，请在手机上确认…"),
                _ => {}
            }
            last_code = Some(code);
        }
        match code {
            0 => {
                // 凭证来源（按可靠性排序）：Set-Cookie 头 → data.url 查询参数
                return cookie_from_credentials(&set_cookies, data.url.as_deref());
            }
            86038 => {
                return Err(AppError::user(format!("二维码已失效：{detail}"))
                    .with_hint("重新运行：bilisum login"));
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
        }
    }
    Err(AppError::user("登录超时").with_hint("重新运行：bilisum login"))
}

/// 需要收集的凭证字段。
const CREDENTIAL_KEYS: [&str; 5] = [
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "sid",
];

/// 组装 Netscape cookies.txt 内容。
///
/// 凭证有两个来源，按可靠性排序：
/// 1. `Set-Cookie` 响应头（B站官方文档说明登录成功后会设置这些 cookie）
/// 2. 成功响应 `data.url` 的查询参数（跨域 SSO 跳转地址）
fn cookie_from_credentials(set_cookies: &[String], login_url: Option<&str>) -> Result<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    for header in set_cookies {
        // `Set-Cookie: NAME=VALUE; Path=/; Domain=...`
        let Some((name_value, _)) = header.split_once(';') else {
            continue;
        };
        let Some((name, value)) = name_value.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if CREDENTIAL_KEYS.contains(&name) && !value.trim().is_empty() {
            pairs.push((name.to_string(), value.trim().to_string()));
        }
    }

    // Set-Cookie 未覆盖全部字段时，用 url 查询参数补齐（不覆盖已有值）
    if let Some(login_url) = login_url {
        if let Ok(parsed) = reqwest::Url::parse(login_url) {
            for (key, value) in parsed.query_pairs() {
                if !CREDENTIAL_KEYS.contains(&key.as_ref()) || value.trim().is_empty() {
                    continue;
                }
                if !pairs.iter().any(|(name, _)| name == key.as_ref()) {
                    pairs.push((key.to_string(), value.trim().to_string()));
                }
            }
        }
    }

    if pairs.is_empty() {
        return Err(AppError::system("登录成功但未获取到凭证")
            .with_hint("B站接口可能已变更，请重试 bilisum login"));
    }

    let mut content = String::from("# Netscape HTTP Cookie File\n");
    for (name, value) in &pairs {
        content.push_str(&format!(
            ".bilibili.com\tTRUE\t/\tFALSE\t0\t{name}\t{value}\n"
        ));
    }
    Ok(content)
}

/// 退出登录（`bili_jct` 作 CSRF；失败不抛错）。
pub async fn logout(http: &HttpClient, cookie: &str) -> Result<()> {
    let csrf = extract_bili_jct(cookie).unwrap_or_default();
    let body = format!("biliCSRF={csrf}");
    let headers = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Cookie", cookie),
    ];
    let _ = http
        .post_form(
            "https://passport.bilibili.com/login/exit/v2",
            &headers,
            &body,
        )
        .await;
    Ok(())
}

/// 从 cookie 串提取 `bili_jct`。
pub fn extract_bili_jct(cookie: &str) -> Option<String> {
    for part in cookie.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("bili_jct=") {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_are_distinct() {
        let active = SessionStatus::Active {
            name: "猫娘".to_string(),
            uid: 42,
        };
        assert!(active.label().contains("已登录"));
        assert!(active.label().contains("猫娘"));
        assert_eq!(SessionStatus::SignedOut.label(), "未登录");
        assert_eq!(SessionStatus::Expired.label(), "已过期");
        assert!(SessionStatus::ServiceError("超时".into())
            .label()
            .contains("服务异常"));
    }

    #[test]
    fn nav_response_uses_camel_case_is_login() {
        // B站 nav 返回 `isLogin`（camelCase）；若字段名不匹配会被 #[serde(default)]
        // 静默降级为 false，导致误判「已过期」——回归测试锁死这个契约。
        let body = r#"{"code":0,"message":"OK","data":{"isLogin":true,"uname":"星夜月fufu","mid":182771025}}"#;
        let parsed: NavResponse = serde_json::from_str(body).unwrap();
        let data = parsed.data.unwrap();
        assert!(data.is_login, "isLogin 必须被正确解析");
        assert_eq!(data.uname.as_deref(), Some("星夜月fufu"));
        assert_eq!(data.mid, Some(182771025));
    }

    #[test]
    fn nav_response_without_login_is_false() {
        let body = r#"{"code":0,"data":{"isLogin":false}}"#;
        let parsed: NavResponse = serde_json::from_str(body).unwrap();
        assert!(!parsed.data.unwrap().is_login);
    }

    #[test]
    fn nav_response_minus_101_is_expired() {
        let body = r#"{"code":-101,"message":"账号未登录"}"#;
        let parsed: NavResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.code, -101);
    }

    #[test]
    fn expired_and_service_error_are_distinct() {
        assert_ne!(SessionStatus::Expired, SessionStatus::ServiceError("x".into()));
    }

    #[test]
    fn extracts_bili_jct() {
        assert_eq!(
            extract_bili_jct("SESSDATA=abc; bili_jct=def; x=y").as_deref(),
            Some("def")
        );
        assert_eq!(extract_bili_jct("SESSDATA=abc").as_deref(), None);
    }

    #[test]
    fn set_cookie_headers_become_netscape_cookie() {
        // B站登录成功后在响应头设置凭证（官方文档）
        let headers = vec![
            "SESSDATA=s1; Path=/; Domain=bilibili.com; HttpOnly; Secure".to_string(),
            "bili_jct=j1; Path=/; Domain=bilibili.com".to_string(),
            "DedeUserID=99; Path=/; Domain=bilibili.com".to_string(),
            "unrelated=x; Path=/".to_string(),
        ];
        let content = cookie_from_credentials(&headers, None).unwrap();
        assert!(content.starts_with("# Netscape HTTP Cookie File"));
        assert!(content.contains("SESSDATA\ts1"));
        assert!(content.contains("bili_jct\tj1"));
        assert!(content.contains("DedeUserID\t99"));
        assert!(!content.contains("unrelated"));
    }

    #[test]
    fn login_url_is_fallback_source() {
        let url = "https://passport.bilibili.com/x/passport-login/web/sso/login?SESSDATA=s2&bili_jct=j2&DedeUserID=88&sid=xyz&other=ignored";
        let content = cookie_from_credentials(&[], Some(url)).unwrap();
        assert!(content.contains("SESSDATA\ts2"));
        assert!(content.contains("bili_jct\tj2"));
        assert!(content.contains("DedeUserID\t88"));
        assert!(content.contains("sid\txyz"));
        assert!(!content.contains("other"));
    }

    #[test]
    fn set_cookie_wins_and_url_fills_gaps() {
        // Set-Cookie 已有 SESSDATA；url 补齐 bili_jct，且不得覆盖已有值
        let headers = vec!["SESSDATA=from_header; Path=/".to_string()];
        let url = "https://x.y/z?SESSDATA=from_url&bili_jct=from_url";
        let content = cookie_from_credentials(&headers, Some(url)).unwrap();
        assert!(content.contains("SESSDATA\tfrom_header"), "Set-Cookie 应优先");
        assert!(content.contains("bili_jct\tfrom_url"), "url 应补齐缺失项");
        assert!(!content.contains("from_url\t"), "不应产生重复 SESSDATA");
    }

    #[test]
    fn no_credentials_anywhere_is_error() {
        let err = cookie_from_credentials(&[], Some("https://x.y/z?nothing=1")).unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::System);
        let err = cookie_from_credentials(&[], None).unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::System);
    }

    #[test]
    fn qrcode_renders_without_panic() {
        assert!(render_qrcode("https://example.com/login").is_ok());
    }
}

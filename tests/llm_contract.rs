//! LLM 调用契约测试：用本地 mock HTTP 服务验证请求体形状与 custom prompt 注入。
//!
//! 不触外网；服务只监听 127.0.0.1 的随机端口。

use std::io::{Read, Write};
use std::net::TcpListener;

use bilisum::config::LlmConfig;
use bilisum::http::HttpClient;
use bilisum::llm;
use bilisum::mode::Mode;
use bilisum::types::{Segment, Transcript, TranscriptSource};

/// 启动一次性 mock 服务，返回 (base_url, 收到的请求体)。
fn mock_server(response_body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定端口");
    let addr = listener.local_addr().expect("本地地址");
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 65536];
            let mut total = Vec::new();
            // 必须读到 Content-Length 声明的完整 body：请求常常分包到达，只读一轮会断言失败
            loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                total.extend_from_slice(&buf[..n]);
                if request_complete(&total) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&total).to_string();
            tx.send(request).ok();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://{addr}"), rx)
}

/// 判断已收到的字节是否包含完整的 HTTP 请求（header + Content-Length 声明的 body）。
fn request_complete(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let Some(split) = text.find("\r\n\r\n") else {
        return false;
    };
    let head = text[..split].to_ascii_lowercase();
    let body_len = bytes.len() - (split + 4);
    match head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        Some(expected) => body_len >= expected,
        // 没有 Content-Length：header 收全即可
        None => true,
    }
}

const OK_RESPONSE: &str = r#"{"choices":[{"message":{"content":"润色后的字幕"}}]}"#;

#[tokio::test]
async fn custom_prompt_reaches_llm_request_body() {
    let (base_url, rx) = mock_server(OK_RESPONSE);
    let http = HttpClient::new().unwrap();
    let config = LlmConfig {
        api_key: "sk-test".to_string(),
        base_url,
        model: "test-model".to_string(),
        reasoning_effort: "none".to_string(),
    };

    let prompt = "自定义模板 {{title}}\n正文 {{transcript}}";
    // 不传 max_tokens → 应使用 16k 默认预算
    let reply = llm::call(&http, &config, prompt, None).await.unwrap();
    assert_eq!(reply, "润色后的字幕");

    let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    // 请求路径必须由 base_url 推导
    assert!(
        request.starts_with("POST /chat/completions"),
        "请求行异常: {}",
        request.lines().next().unwrap_or("")
    );
    // 鉴权头
    assert!(request.contains("authorization: Bearer sk-test") || request.contains("Authorization: Bearer sk-test"));
    // 模型与 prompt 必须出现在 body
    assert!(request.contains("test-model"));
    assert!(request.contains("自定义模板"));
    // 输出上限必须给推理模型留足预算（16k），否则 content 会被截断为空
    assert!(request.contains("\"max_tokens\":16000"), "应使用 16k 上限: {request}");
    // `none` 也不下发：始终思考的模型（GLM）收到 "none" 会直接 400
    assert!(
        !request.contains("reasoning_effort"),
        "none 不应下发 reasoning_effort: {request}"
    );
}

#[tokio::test]
async fn empty_reasoning_effort_is_omitted() {
    // 不支持该字段的端点：留空即不下发，避免 400
    let (base_url, rx) = mock_server(OK_RESPONSE);
    let http = HttpClient::new().unwrap();
    let config = LlmConfig {
        api_key: "sk-test".to_string(),
        base_url,
        model: "m".to_string(),
        reasoning_effort: String::new(),
    };
    llm::call(&http, &config, "x", None).await.unwrap();
    let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(
        !request.contains("reasoning_effort"),
        "空值不应下发: {request}"
    );
}

#[tokio::test]
async fn explicit_max_tokens_overrides_default() {
    let (base_url, rx) = mock_server(OK_RESPONSE);
    let http = HttpClient::new().unwrap();
    let config = LlmConfig {
        api_key: "sk-test".to_string(),
        base_url,
        model: "m".to_string(),
        reasoning_effort: "none".to_string(),
    };
    llm::call(&http, &config, "x", Some(64)).await.unwrap();
    let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(request.contains("\"max_tokens\":64"), "显式值应覆盖默认: {request}");
}

#[tokio::test]
async fn endpoint_derives_chat_completions_path() {
    let (base_url, _rx) = mock_server(OK_RESPONSE);
    let endpoint = llm::resolve_endpoint(&base_url);
    assert_eq!(endpoint.url, format!("{base_url}/chat/completions"));
}

#[tokio::test]
async fn http_error_body_surfaces_server_message() {
    // 服务返回 401 与错误体，验证错误消息包含服务端说明
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf);
            let body = r#"{"error":{"message":"invalid api key"}}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    let http = HttpClient::new().unwrap();
    let config = LlmConfig {
        api_key: "sk-bad".to_string(),
        base_url: format!("http://{addr}"),
        model: "m".to_string(),
        reasoning_effort: "none".to_string(),
    };
    let err = llm::call(&http, &config, "x", None).await.unwrap_err();
    assert_eq!(err.class, bilisum::error::ExitClass::System);
    assert!(
        err.message.contains("invalid api key"),
        "应包含服务端消息: {}",
        err.message
    );
}

#[tokio::test]
async fn http_client_captures_set_cookie_headers() {
    // B站登录凭证只出现在 Set-Cookie 头，必须能被拿到
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf);
            let body = r#"{"code":0}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Set-Cookie: SESSDATA=sess123; Path=/; Domain=bilibili.com; HttpOnly\r\n\
                 Set-Cookie: bili_jct=jct456; Path=/; Domain=bilibili.com\r\n\
                 Set-Cookie: DedeUserID=999; Path=/\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    let http = HttpClient::new().unwrap();
    let resp = http.get(&format!("http://{addr}/poll")).await.unwrap();
    let cookies = resp.set_cookies();
    assert_eq!(cookies.len(), 3, "应捕获全部 Set-Cookie: {cookies:?}");
    assert!(cookies[0].starts_with("SESSDATA=sess123"));
    assert!(cookies[1].starts_with("bili_jct=jct456"));
    assert!(cookies[2].starts_with("DedeUserID=999"));
}

#[tokio::test]
async fn polish_sends_the_entire_raw_text_in_one_request() {
    let (base_url, rx) = mock_server(
        r#"{"choices":[{"message":{"content":"整段润色结果"}}]}"#,
    );
    let http = HttpClient::new().unwrap();
    let config = bilisum::config::Config {
        llm: LlmConfig {
            api_key: "sk-test".to_string(),
            base_url,
            model: "test-model".to_string(),
            reasoning_effort: "none".to_string(),
        },
        ..Default::default()
    };
    let transcript = Transcript::from_segments(
        vec![
            Segment::new(0.0, 1.0, "第一段"),
            Segment::new(1.0, 2.0, "第二段"),
            Segment::new(2.0, 3.0, "第三段"),
        ],
        TranscriptSource::Whisper,
    );

    let result = bilisum::app::generate(
        bilisum::app::GenerateInput {
            transcript: &transcript,
            title: "测试标题",
            url: "https://example.test/video",
            mode: Mode::Polish,
            custom_prompt: "",
            config: &config,
            http: &http,
        },
        &|_| {},
    )
    .await
    .unwrap();

    assert_eq!(result.text, "整段润色结果");
    let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(request.contains("第一段\\n第二段\\n第三段"));
    assert!(!request.contains("1. 第一段"));
    assert!(!request.contains("2. 第二段"));
}

/// 第一次响应故意在 chunked 传输中途断开（模拟 90s 静默期被中间节点掐断），第二次正常返回。
fn truncated_then_ok_server(
    response_body: &'static str,
) -> (String, std::sync::mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定端口");
    let addr = listener.local_addr().expect("本地地址");
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        // 第一次：不完整的 chunked 响应（缺终止块）
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"cho",
            );
            let _ = stream.flush();
            // 不写终止块，直接关闭
        }
        // 第二次：正常响应
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            tx.send(()).ok();
        }
    });

    (format!("http://{addr}"), rx)
}

#[tokio::test]
async fn truncated_body_is_retried() {
    let (base_url, rx) = truncated_then_ok_server(OK_RESPONSE);
    let http = HttpClient::new().unwrap();
    let config = LlmConfig {
        api_key: "sk-test".to_string(),
        base_url,
        model: "m".to_string(),
        reasoning_effort: String::new(),
    };
    let reply = llm::call(&http, &config, "x", None)
        .await
        .expect("截断后应重试成功");
    assert_eq!(reply, "润色后的字幕");
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("第二次请求应到达");
}

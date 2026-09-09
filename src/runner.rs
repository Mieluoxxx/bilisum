//! 外部进程执行（yt-dlp / ffmpeg）。
//!
//! 逐行转发 stdout/stderr 到回调，供 CLI 打印进度（TUI 亦可复用同一回调）。

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::{AppError, Result};

/// 子进程执行结果。
#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RunResult {
    /// 成功（退出码 0）。
    pub fn is_ok(&self) -> bool {
        self.exit_code == 0
    }

    /// stderr 末尾若干行（错误提示用）。
    pub fn stderr_tail(&self, max_lines: usize) -> String {
        let lines: Vec<&str> = self.stderr.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }
}

/// 执行外部命令，逐行回调。
///
/// 用 `tokio::join!` 在同一任务内并发读取 stdout/stderr，因此回调可以借用外部状态
/// （无需 `'static`），也避免了 spawn 带来的额外开销。
pub async fn run(
    program: &str,
    args: &[String],
    on_line: impl Fn(&str) + Send + Sync,
) -> Result<RunResult> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AppError::missing_binary(program, &install_hint(program))
        } else {
            AppError::system(format!("启动 {program} 失败：{err}"))
        }
    })?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let read_stdout = read_lines(stdout, &on_line);
    let read_stderr = read_lines(stderr, &on_line);
    let wait = child.wait();

    let (stdout, stderr, status) = tokio::join!(read_stdout, read_stderr, wait);
    let status = status.map_err(|err| AppError::system(format!("等待 {program} 失败：{err}")))?;

    Ok(RunResult {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

/// 逐行读取并回传，返回完整内容。
async fn read_lines(
    stream: impl tokio::io::AsyncRead + Unpin,
    on_line: &(impl Fn(&str) + Send + Sync),
) -> String {
    let mut collected = String::new();
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        on_line(&line);
        collected.push_str(&line);
        collected.push('\n');
    }
    collected
}

/// 二进制是否在 PATH 中可用（`bilisum doctor` 用）。
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 安装提示。
pub fn install_hint(program: &str) -> String {
    match program {
        "yt-dlp" => "brew install yt-dlp".to_string(),
        "ffmpeg" => "brew install ffmpeg".to_string(),
        "cmake" => "brew install cmake".to_string(),
        other => format!("请安装 {other} 并确保在 PATH 中"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_echo_and_collects_lines() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = lines.clone();
        let result = run(
            "echo",
            &["hello".to_string()],
            move |line| sink.lock().unwrap().push(line.to_string()),
        )
        .await
        .unwrap();
        assert!(result.is_ok());
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(lines.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_binary_is_system_error_with_hint() {
        let err = run("bilisum-definitely-missing", &[], |_| {}).await.unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::System);
        assert!(err.hint.unwrap().contains("安装"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported() {
        let result = run("false", &[], |_| {}).await.unwrap();
        assert!(!result.is_ok());
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn stderr_tail_keeps_last_lines() {
        let result = RunResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "a\nb\nc\n".to_string(),
        };
        assert_eq!(result.stderr_tail(2), "b\nc");
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("bilisum-not-here").is_none());
    }
}

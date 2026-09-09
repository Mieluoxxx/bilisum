//! 结构化错误与退出码。
//!
//! 退出码契约：`0` 成功 / `1` 用户错误（参数、缺 key、链接无效）/ `2` 系统错误（网络、子进程、缺依赖）。

use std::fmt;

/// 退出码分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// 用户输入或配置问题。
    User,
    /// 外部环境或系统问题。
    System,
}

impl ExitClass {
    pub fn code(self) -> i32 {
        match self {
            ExitClass::User => 1,
            ExitClass::System => 2,
        }
    }
}

/// 应用错误：人话消息 + 退出码分类。
#[derive(Debug)]
pub struct AppError {
    pub class: ExitClass,
    pub message: String,
    pub hint: Option<String>,
}

impl AppError {
    pub fn user(message: impl Into<String>) -> Self {
        Self {
            class: ExitClass::User,
            message: message.into(),
            hint: None,
        }
    }

    pub fn system(message: impl Into<String>) -> Self {
        Self {
            class: ExitClass::System,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 缺少外部二进制（系统错误 + 安装提示）。
    pub fn missing_binary(program: &str, install: &str) -> Self {
        Self::system(format!("未找到 {program}"))
            .with_hint(format!("安装：{install}"))
    }

    /// 未配置 API Key（用户错误 + 配置提示）。
    pub fn missing_api_key() -> Self {
        Self::user("未配置 LLM API Key，无法调用大模型")
            .with_hint("配置：bilisum settings")
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n提示：{hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        // anyhow 链已含上下文；按系统错误处理（IO/网络/解析等）
        AppError::system(format!("{err:#}"))
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

/// 把 `anyhow::Result` 转成系统类 `AppError`。
pub trait IntoAppResult<T> {
    fn sys_context(self, context: impl fmt::Display) -> Result<T>;
}

impl<T> IntoAppResult<T> for anyhow::Result<T> {
    fn sys_context(self, context: impl fmt::Display) -> Result<T> {
        self.map_err(|err| AppError::system(format!("{context}：{err:#}")))
    }
}

impl<T> IntoAppResult<T> for std::io::Result<T> {
    fn sys_context(self, context: impl fmt::Display) -> Result<T> {
        self.map_err(|err| AppError::system(format!("{context}：{err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(ExitClass::User.code(), 1);
        assert_eq!(ExitClass::System.code(), 2);
    }

    #[test]
    fn hint_renders_on_second_line() {
        let err = AppError::missing_binary("yt-dlp", "brew install yt-dlp");
        let text = err.to_string();
        assert!(text.contains("未找到 yt-dlp"));
        assert!(text.contains("提示：安装：brew install yt-dlp"));
        assert_eq!(err.class, ExitClass::System);
    }
}

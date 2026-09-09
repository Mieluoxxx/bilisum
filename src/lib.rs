//! bilisum 库入口：核心逻辑在此，`main.rs` 只做 CLI 解析与输出。

pub mod app;
pub mod auth;
pub mod doctor;
pub mod models;
pub mod cache;
pub mod config;
pub mod error;
pub mod http;
pub mod llm;
pub mod mode;
pub mod paths;
pub mod platform;
pub mod prompt;
pub mod render;
pub mod runner;
pub mod session;
pub mod stt_models;
pub mod subtitle;
#[cfg(test)]
pub mod test_support;
pub mod types;
pub mod whisper;

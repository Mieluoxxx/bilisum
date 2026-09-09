//! CLI 端到端测试：退出码、配置读写、session / cache 子命令。
//!
//! 每个测试用独立 `BILISUM_HOME`（临时目录），互不影响。

use std::path::PathBuf;
use std::process::{Command, Output};

/// 运行 `bilisum` 并返回输出，环境隔离到 `home`。
fn bilisum(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bilisum"))
        .args(args)
        .env("BILISUM_HOME", home)
        .env("NO_COLOR", "1")
        .output()
        .expect("执行 bilisum")
}

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bilisum-cli-{tag}-{}",
        std::process::id() as u64 * 1000 + std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    ));
    std::fs::create_dir_all(&dir).expect("创建临时 home");
    dir
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn help_lists_every_documented_subcommand() {
    let home = temp_home("help");
    let output = bilisum(&home, &["--help"]);
    assert!(output.status.success());
    let text = stdout(&output);
    for command in [
        "settings", "login", "whoami", "models", "sessions", "cache", "doctor",
    ] {
        assert!(text.contains(command), "help 缺少子命令: {command}");
    }
    // 顶层参数契约
    for flag in [
        "--continue", "--mode", "--source", "--output", "--json", "--force", "--no-cache",
    ] {
        assert!(text.contains(flag), "help 缺少参数: {flag}");
    }
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn cache_help_lists_all_cache_commands() {
    let home = temp_home("cachehelp");
    let output = bilisum(&home, &["cache", "--help"]);
    assert!(output.status.success());
    let text = stdout(&output);
    for command in ["ls", "size", "path", "rm", "prune", "clean"] {
        assert!(text.contains(command), "cache help 缺少命令: {command}");
    }

    let output = bilisum(&home, &["cache", "ls", "--help"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("Usage:"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn settings_set_persists_and_redacts_secret() {
    let home = temp_home("settings");
    let output = bilisum(&home, &["settings", "set", "llm.api_key", "sk-secret-1234"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    // 输出必须脱敏
    let out = stdout(&output);
    assert!(!out.contains("secret"), "API Key 未脱敏: {out}");
    assert!(out.contains("sk-s"), "应显示前 4 位: {out}");

    // 落盘为明文（工具自身要能读回）
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("sk-secret-1234"));
    assert!(config.contains("[llm]"));
    assert!(config.contains("[stt]"));
    assert!(config.contains("[output]"));
    assert!(config.contains("[cache]"));

    // 读回：再设一次其他键，确认不覆盖已有值
    let output = bilisum(&home, &["settings", "set", "output.default_mode", "summary"]);
    assert!(output.status.success());
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("sk-secret-1234"), "已有值被覆盖");
    assert!(config.contains("default_mode = \"summary\""));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn invalid_config_value_is_user_error_exit_1() {
    let home = temp_home("badkey");
    let output = bilisum(&home, &["settings", "set", "output.default_mode", "bogus"]);
    assert_eq!(output.status.code(), Some(1), "应为用户错误退出码");
    assert!(stderr(&output).contains("未知模式"));

    let output = bilisum(&home, &["settings", "set", "nope.nope", "x"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("未知配置项"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn missing_url_without_continue_is_user_error() {
    let home = temp_home("nourl");
    let output = bilisum(&home, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("缺少视频链接"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn unsupported_url_is_user_error() {
    let home = temp_home("badurl");
    let output = bilisum(&home, &["https://vimeo.com/123", "--mode", "transcript"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("暂不支持"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn invalid_mode_and_source_are_validated_before_session_lookup() {
    let home = temp_home("argcheck");
    // 参数校验必须早于 session 查找：否则报错信息会误导
    let output = bilisum(&home, &["-c", "--mode", "bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("未知模式"), "实际: {}", stderr(&output));
    assert!(!stderr(&output).contains("还没有任何 session"));

    let output = bilisum(&home, &["-c", "--source", "bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("无效的字幕来源"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn continue_without_sessions_is_user_error() {
    let home = temp_home("nosess");
    let output = bilisum(&home, &["-c"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("还没有任何 session"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn sessions_ls_empty_and_cache_ls_empty() {
    let home = temp_home("empty");
    let output = bilisum(&home, &["sessions", "ls"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("还没有任何 session"));

    let output = bilisum(&home, &["cache", "ls"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("缓存为空"));

    let output = bilisum(&home, &["cache", "size"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("当前占用"));

    let output = bilisum(&home, &["cache", "path", "BVEMPTY"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("BVEMPTY.wav"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn cache_rm_and_prune_manage_cached_files() {
    let home = temp_home("cachecommands");
    let cache_dir = home.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("BVREMOVE.wav"), b"audio").unwrap();

    let output = bilisum(&home, &["cache", "rm", "BVREMOVE"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(!cache_dir.join("BVREMOVE.wav").exists());

    let output = bilisum(&home, &["cache", "rm", "BVREMOVE"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("缓存不存在"));

    let output = bilisum(&home, &["cache", "prune"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("按 LRU 清理"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn view_lists_and_reads_cached_content_by_url_or_bv() {
    let home = temp_home("view");
    let session_dir = home.join("sessions").join("BV1VIEW");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("meta.json"),
        r#"{"id":"BV1VIEW","url":"https://www.bilibili.com/video/BV1VIEW","title":"缓存视频","platform":"bilibili","source":"whisper","mode":"polish","model":"","created_at":1700000000,"audio_cache_key":"BV1VIEW"}"#,
    )
    .unwrap();
    std::fs::write(session_dir.join("raw.txt"), "原始字幕\n").unwrap();
    std::fs::write(session_dir.join("llm.txt"), "润色结果\n").unwrap();

    let output = bilisum(&home, &["view", "ls"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("BV1VIEW"));
    assert!(stdout(&output).contains("llm.txt"));

    let output = bilisum(&home, &["view", "BV1VIEW"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "润色结果\n");

    let output = bilisum(
        &home,
        &["view", "https://www.bilibili.com/video/BV1VIEW"],
    );
    assert!(output.status.success());
    assert_eq!(stdout(&output), "润色结果\n");
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn cache_clean_reports_zero_on_empty_cache() {
    let home = temp_home("cleancache");
    let output = bilisum(&home, &["cache", "clean"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("已清理 0 个文件"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn models_download_unknown_model_is_user_error() {
    let home = temp_home("badmodel");
    let output = bilisum(&home, &["models", "download", "ggml-nope.bin"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("未知的 STT 模型"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn sessions_rm_unknown_id_is_user_error() {
    let home = temp_home("rmunknown");
    let output = bilisum(&home, &["sessions", "rm", "not-a-session"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("session 不存在"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn settings_test_without_api_key_is_user_error() {
    let home = temp_home("testkey");
    let output = bilisum(&home, &["settings", "test"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("未配置 LLM API Key"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn doctor_reports_missing_prerequisites_as_system_error() {
    // 用一个空 PATH 保证外部二进制检测失败；退出码应为 2（系统错误）
    let home = temp_home("doctor");
    let output = Command::new(env!("CARGO_BIN_EXE_bilisum"))
        .arg("doctor")
        .env("BILISUM_HOME", &home)
        .env("PATH", "/nonexistent")
        .env("NO_COLOR", "1")
        .output()
        .expect("执行 doctor");
    let text = stdout(&output);
    assert!(text.contains("环境自检"));
    assert!(text.contains("yt-dlp"));
    assert!(text.contains("ffmpeg"));
    assert_eq!(output.status.code(), Some(2), "缺前置应为系统错误");
    std::fs::remove_dir_all(&home).ok();
}

//! CLI 入口：参数解析、输出与退出码。
//!
//! 业务逻辑全在库中（`app` / `session` / `cache` …）；本文件只负责 I/O。

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result as AnyResult;
use clap::{Args, Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};

use bilisum::app::{self, ProgressEvent, RunOptions};
use bilisum::config::Config;
use bilisum::error::{AppError, ExitClass, Result};
use bilisum::http::HttpClient;
use bilisum::mode::Mode;
use bilisum::paths;
use bilisum::prompt::LEGACY_PROMPT;
use bilisum::session::{self, Session};
use bilisum::stt_models;
use bilisum::types::format_local_time;
use bilisum::{auth, doctor, models};

#[derive(Parser)]
#[command(
    name = "bilisum",
    version,
    about = "B站 / YouTube 视频字幕与摘要工具",
    long_about = None,
)]
struct Cli {
    /// 视频链接（省略时需配合 -c 续跑已有 session）
    url: Option<String>,

    /// 续跑 session：`-c` 用最近一次，`-c <id>` 指定
    #[arg(short = 'c', long = "continue", value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    continue_session: Option<String>,

    /// 输出模式
    #[arg(short = 'm', long = "mode", value_name = "MODE")]
    mode: Option<String>,

    /// 字幕来源：audio（默认）| subtitle
    #[arg(short = 's', long = "source", value_name = "SOURCE")]
    source: Option<String>,

    /// 输出到文件（省略时写 stdout）
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// 机器可读 JSON 输出
    #[arg(long = "json")]
    json: bool,

    /// 丢弃已有 raw.txt，重跑阶段 A（复用音频缓存）
    #[arg(short = 'f', long = "force")]
    force: bool,

    /// 连音频也重新下载（不使用缓存）
    #[arg(long = "no-cache")]
    no_cache: bool,

    /// 详细日志（可重复：-v / -vv）
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,

    /// 日志落盘到 ~/.bili/logs/
    #[arg(long = "log")]
    log: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 配置管理
    Settings(SettingsArgs),
    /// B 站扫码登录
    Login,
    /// 查看登录状态
    Whoami,
    /// STT 模型列表 / 下载
    Models(ModelsArgs),
    /// session 历史
    Sessions(SessionsArgs),
    /// 查看已缓存内容
    View(ViewArgs),
    /// 音频缓存
    Cache(CacheArgs),
    /// 环境自检
    Doctor,
}

#[derive(Args)]
struct SettingsArgs {
    #[command(subcommand)]
    action: Option<SettingsAction>,
}

#[derive(Subcommand)]
enum SettingsAction {
    /// 修改单个配置项：settings set llm.model gpt-4o-mini
    Set {
        /// 配置项（如 llm.api_key）
        key: String,
        /// 值
        value: String,
    },
    /// 用 $EDITOR 编辑 custom.prompt
    Edit,
    /// 测试大模型连通性
    Test,
}

#[derive(Args)]
struct ModelsArgs {
    #[command(subcommand)]
    action: Option<ModelsAction>,
}

#[derive(Subcommand)]
enum ModelsAction {
    /// 下载模型：models download <model-file>
    Download {
        /// 模型 id（省略则交互选择）
        id: Option<String>,
        /// 下载镜像：huggingface | hf-mirror（省略则交互选择）
        #[arg(long = "mirror", value_name = "MIRROR")]
        mirror: Option<String>,
    },
}

#[derive(Args)]
struct SessionsArgs {
    #[command(subcommand)]
    action: Option<SessionsAction>,
}

#[derive(Subcommand)]
enum SessionsAction {
    /// 列出全部 session（默认）
    Ls,
    /// 查看某个 session 详情
    Show { id: Option<String> },
    /// 删除某个 session
    Rm { id: String },
}

#[derive(Args)]
struct ViewArgs {
    /// `ls` 列出缓存；传视频 URL 或 BV 号则显示对应 session 内容
    target: Option<String>,
}

#[derive(Args)]
struct CacheArgs {
    #[command(subcommand)]
    action: Option<CacheAction>,
}

#[derive(Subcommand)]
enum CacheAction {
    /// 列出缓存文件与占用
    Ls,
    /// 显示缓存总占用与配置上限
    Size,
    /// 显示指定视频缓存的绝对路径
    Path { video_key: String },
    /// 删除指定视频缓存
    Rm { video_key: String },
    /// 按 LRU 上限清理旧缓存
    Prune,
    /// 清空缓存
    Clean,
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("✗ {err}");
        std::process::exit(err.class.code());
    }
}

fn run(cli: Cli) -> Result<()> {
    init_tracing(cli.verbose, cli.log)?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| AppError::system(format!("初始化运行时失败：{err}")))?;

    if let Some(command) = cli.command {
        return runtime.block_on(dispatch(command));
    }

    // 顶层任务：处理 URL 或续跑 session
    runtime.block_on(run_task(&cli))
}

fn init_tracing(verbose: u8, log: bool) -> Result<()> {
    use tracing_subscriber::EnvFilter;

    // 控制台默认静默：面向用户的消息由 CLI 自己打印（进度 + 最终错误），
    // tracing 只作为 -v/-vv 的额外诊断。--log 时始终以 debug 落盘。
    // -v 开 info（阶段与关键决策），-vv 开 debug。
    let console_level = match verbose {
        0 => "off",
        1 => "info",
        _ => "debug",
    };
    let file_level = "debug";

    if log {
        paths::ensure_layout().map_err(|err| AppError::system(err.to_string()))?;
        let dir = paths::logs_dir().map_err(|err| AppError::system(err.to_string()))?;
        let file = tracing_appender::rolling::daily(&dir, "bilisum.log");
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("bilisum={file_level}")));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .with_ansi(false)
            .try_init()
            .ok();
    } else {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("bilisum={console_level}")));
        // 非 TTY（管道/重定向）时禁用 ANSI，避免污染日志与 grep 结果
        let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(ansi)
            .try_init()
            .ok();
    }
    Ok(())
}

async fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Settings(args) => settings(args).await,
        Command::Login => auth::login().await,
        Command::Whoami => auth::whoami().await,
        Command::Models(args) => match args.action {
            // 显式指定：models download <id> [--mirror <m>]
            Some(ModelsAction::Download { id, mirror }) => {
                // 先校验模型名（错误应尽早暴露，不该先弹交互菜单）
                if let Some(value) = &id {
                    validate_model_id(value)?;
                }
                let (id, already) = match id {
                    Some(id) => {
                        let already = paths::model_file(&id)
                            .map(|path| path.is_file())
                            .unwrap_or(false);
                        (id, already)
                    }
                    None => choose_model()?,
                };
                if already {
                    println!("✓ {id} 已存在，无需下载");
                    return Ok(());
                }
                let mirror = match mirror {
                    Some(value) => stt_models::Mirror::parse(&value).ok_or_else(|| {
                        AppError::user(format!("未知镜像: {value}"))
                            .with_hint("可用：huggingface, hf-mirror")
                    })?,
                    None => choose_mirror()?,
                };
                models::download(&id, mirror, &cli_progress()).await
            }
            // 无子命令：选模型 →（未下载才）选镜像 → 自动下载
            None => {
                let (id, already) = choose_model()?;
                if already {
                    println!("✓ {id} 已存在，无需下载");
                    return Ok(());
                }
                let mirror = choose_mirror()?;
                models::download(&id, mirror, &cli_progress()).await
            }
        },
        Command::Sessions(args) => match args.action.unwrap_or(SessionsAction::Ls) {
            SessionsAction::Ls => sessions_ls(),
            SessionsAction::Show { id } => sessions_show(id.as_deref()),
            SessionsAction::Rm { id } => {
                let session = Session::open(&id)?;
                session.remove()?;
                println!("✓ 已删除 session {id}");
                Ok(())
            }
        },
        Command::View(args) => view_cached(args.target.as_deref()),
        Command::Cache(args) => match args.action.unwrap_or(CacheAction::Ls) {
            CacheAction::Ls => cache_ls(),
            CacheAction::Size => cache_size(),
            CacheAction::Path { video_key } => cache_path(&video_key),
            CacheAction::Rm { video_key } => cache_rm(&video_key),
            CacheAction::Prune => cache_prune(),
            CacheAction::Clean => {
                let (removed, freed) = bilisum::cache::clean()?;
                println!(
                    "✓ 已清理 {removed} 个文件，释放 {}",
                    paths::human_size(freed)
                );
                Ok(())
            }
        },
        Command::Doctor => doctor::run().await,
    }
}

/// 查看缓存内容：优先 LLM 产物，没有则回退原始字幕。
fn view_cached(target: Option<&str>) -> Result<()> {
    match target {
        None | Some("ls") => view_cached_list(),
        Some(value) => view_cached_video(value),
    }
}

fn view_cached_list() -> Result<()> {
    let sessions = session::list()?;
    let cached: Vec<_> = sessions
        .into_iter()
        .filter_map(|session| {
            let artifact = if session.llm_path().is_file() {
                "llm.txt"
            } else if session.raw_path().is_file() {
                "raw.txt"
            } else {
                return None;
            };
            Some((session, artifact))
        })
        .collect();

    if cached.is_empty() {
        println!("还没有可查看的缓存内容");
        return Ok(());
    }
    println!("{:<28} {:<10} 标题", "SESSION", "内容");
    for (session, artifact) in cached {
        println!(
            "{:<28} {:<10} {}",
            session.id,
            artifact,
            truncate(&session.meta.title, 40)
        );
    }
    Ok(())
}

fn view_cached_video(target: &str) -> Result<()> {
    let is_url = target.starts_with("http://") || target.starts_with("https://");
    let session_id = if is_url {
        let _ = bilisum::platform::detect(target)?;
        bilisum::platform::video_key(target)
    } else {
        if !session::is_valid_id(target) {
            return Err(AppError::user(format!("非法 session ID: {target}")));
        }
        target.to_string()
    };
    let session = Session::open(&session_id).map_err(|_| {
        AppError::user(format!("没有找到该视频的缓存内容: {target}"))
            .with_hint("查看缓存列表：bilisum view ls")
    })?;
    let (path, content) = if session.llm_path().is_file() {
        let path = session.llm_path();
        let content = std::fs::read_to_string(&path)
            .map_err(|err| AppError::system(format!("读取缓存失败: {}：{err}", path.display())))?;
        (path, content)
    } else if session.raw_path().is_file() {
        let path = session.raw_path();
        let content = std::fs::read_to_string(&path)
            .map_err(|err| AppError::system(format!("读取缓存失败: {}：{err}", path.display())))?;
        (path, content)
    } else {
        return Err(AppError::user(format!("session 没有可查看的内容: {session_id}")));
    };

    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    eprintln!("\n缓存来源：{}", path.display());
    Ok(())
}

/// 顶层任务：`bilisum <url>` 或 `bilisum -c [id]`。
async fn run_task(cli: &Cli) -> Result<()> {
    // 先校验参数（模式/来源），再碰磁盘与网络：错误应尽早暴露
    if let Some(mode) = cli.mode.as_deref() {
        Mode::parse(mode)?;
    }
    if let Some(source) = cli.source.as_deref() {
        if source != "audio" && source != "subtitle" {
            return Err(AppError::user(format!("无效的字幕来源: {source}"))
                .with_hint("可用：audio, subtitle"));
        }
    }

    let config = app::bootstrap()?;
    let http = HttpClient::new()?;
    let progress = cli_progress();
    let progress: app::ProgressFn<'_> = &*progress;

    let (session, title, transcript, reused) = if let Some(target) = &cli.continue_session {
        let session = if target.is_empty() {
            Session::latest()?
        } else {
            if !session::is_valid_id(target) {
                return Err(AppError::user(format!("非法 session id: {target}")));
            }
            Session::open(target)?
        };
        // `-f`：丢弃 raw.txt 重跑阶段 A（需要 URL）
        if cli.force {
            let options = run_options(cli, &config)?;
            eprintln!("· 重新抓取字幕（-f，覆盖该 session 的 raw.txt）");
            // 覆盖同一个 session，不新建目录
            let result = app::prepare_into(
                &session.meta.url,
                &options,
                &config,
                &http,
                progress,
                Some(&session),
            )
            .await?;
            let (title, transcript) = app::reuse(&result.session)?;
            (result.session, title, transcript, false)
        } else {
            let (title, transcript) = app::reuse(&session)?;
            (session, title, transcript, true)
        }
    } else {
        let url = cli
            .url
            .as_ref()
            .ok_or_else(|| {
                AppError::user("缺少视频链接")
                    .with_hint("用法：bilisum <url>，或续跑：bilisum -c [session_id]")
            })?;
        let options = run_options(cli, &config)?;
        let result = app::prepare(url, &options, &config, &http, progress).await?;
        let (title, transcript) = app::reuse(&result.session)?;
        (result.session, title, transcript, false)
    };

    let mode = cli
        .mode
        .as_deref()
        .map(Mode::parse)
        .transpose()?
        .unwrap_or_else(|| config.default_mode());

    let mut session = session;
    session.meta.mode = mode.as_str().to_string();

    // 阶段 B
    let generated = match app::generate(
        app::GenerateInput {
            transcript: &transcript,
            title: &title,
            url: &session.meta.url,
            mode,
            custom_prompt: &config.custom.prompt,
            config: &config,
            http: &http,
        },
        progress,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            // 无 API Key：阶段 A 已完成，raw.txt 保留
            if err.class == ExitClass::User {
                session.save_meta().ok();
                eprintln!("! 未生成 LLM 产物：{err}");
                eprintln!(
                    "  原始字幕已保存：{}",
                    session.raw_path().display()
                );
                eprintln!(
                    "  配置后重新生成：bilisum -c {} --mode {}",
                    session.id,
                    mode.as_str()
                );
            }
            return Err(err);
        }
    };

    session.write_llm(&generated.text)?;
    if !generated.model.is_empty() {
        session.meta.model = generated.model.clone();
    }
    session.save_meta()?;

    // 输出
    if let Some(path) = &cli.output {
        std::fs::write(path, &generated.text)
            .map_err(|err| AppError::system(format!("写入 {} 失败：{err}", path.display())))?;
    }
    if cli.json {
        let payload = serde_json::json!({
            "session": session.id,
            "title": title,
            "mode": mode.as_str(),
            "model": generated.model,
            "text": generated.text,
            "reused_raw": reused,
            "session_dir": session.dir.display().to_string(),
            "raw": session.raw_path().display().to_string(),
            "llm": session.llm_path().display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
    } else if cli.output.is_none() {
        println!("{}", generated.text);
    }

    print_summary(&session, &title, mode, cli.output.as_deref());
    Ok(())
}

/// 结束提示（含 session ID 与重新生成命令）。
fn print_summary(session: &Session, title: &str, mode: Mode, output: Option<&std::path::Path>) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr);
    let _ = writeln!(stderr, "✓ 完成  {} · {}", truncate(title, 40), mode.label());
    let _ = writeln!(stderr, "  session  {}", session.id);
    let _ = writeln!(stderr, "  raw      {}", session.raw_path().display());
    let _ = writeln!(stderr, "  llm      {}", session.llm_path().display());
    if let Some(path) = output {
        let _ = writeln!(stderr, "  输出     {}", path.display());
    }
    let _ = writeln!(
        stderr,
        "  重新生成 bilisum -c {} --mode fulltext",
        session.id
    );
}

fn truncate(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    format!("{}…", chars[..max_chars].iter().collect::<String>())
}

fn run_options(cli: &Cli, config: &Config) -> Result<RunOptions> {
    let source = cli
        .source
        .clone()
        .unwrap_or_else(|| config.stt.source.clone());
    let mode = cli
        .mode
        .as_deref()
        .map(Mode::parse)
        .transpose()?
        .unwrap_or_else(|| config.default_mode());
    Ok(RunOptions {
        source,
        mode,
        force: cli.force,
        no_cache: cli.no_cache,
        custom_prompt: config.custom.prompt.clone(),
    })
}

/// CLI 进度回调：写 stderr（stdout 留给产物，保证可管道）。
fn cli_progress() -> Box<dyn Fn(ProgressEvent<'_>) + Send + Sync> {
    Box::new(|event: ProgressEvent<'_>| {
        eprintln!("· [{}] {}", event.stage, event.detail);
    })
}

async fn settings(args: SettingsArgs) -> Result<()> {
    match args.action {
        Some(SettingsAction::Set { key, value }) => {
            let mut config = app::bootstrap()?;
            config.set(&key, &value)?;
            config.save()?;
            println!("✓ {key} = {}", redact(&key, &value));
            Ok(())
        }
        Some(SettingsAction::Edit) => edit_custom_prompt(),
        Some(SettingsAction::Test) => {
            let config = app::bootstrap()?;
            let http = HttpClient::new()?;
            let message = bilisum::llm::test_connection(&http, &config.llm).await?;
            println!("✓ {message}");
            Ok(())
        }
        None => settings_interactive(),
    }
}

/// 问答式配置：逐项回车保留原值。
fn settings_interactive() -> Result<()> {
    let mut config = app::bootstrap()?;
    let theme = ColorfulTheme::default();

    println!("配置向导（回车保留当前值）\n");

    let api_key: String = Password::with_theme(&theme)
        .with_prompt(format!(
            "LLM API Key [{}]",
            if config.has_api_key() { "已配置" } else { "未配置" }
        ))
        .allow_empty_password(true)
        .interact()
        .map_err(prompt_error)?;
    if !api_key.trim().is_empty() {
        config.llm.api_key = api_key.trim().to_string();
    }

    let base_url: String = Input::with_theme(&theme)
        .with_prompt("Base URL（空 = OpenAI 官方）")
        .with_initial_text(&config.llm.base_url)
        .allow_empty(true)
        .interact_text()
        .map_err(prompt_error)?;
    config.llm.base_url = base_url.trim().to_string();

    let model: String = Input::with_theme(&theme)
        .with_prompt("模型")
        .with_initial_text(&config.llm.model)
        .allow_empty(false)
        .interact_text()
        .map_err(prompt_error)?;
    config.llm.model = model.trim().to_string();

    // 推理强度：推理型模型（DeepSeek reasoner 等）默认开推理会拖慢 10 倍以上
    let efforts = [
        "none（关闭推理，默认，最快）",
        "minimal",
        "low",
        "medium",
        "high（最强推理，最慢）",
    ];
    let effort_index = bilisum::config::REASONING_EFFORTS
        .iter()
        .position(|value| *value == config.llm.reasoning_effort)
        .unwrap_or(0);
    let effort = Select::with_theme(&theme)
        .with_prompt("推理强度")
        .items(efforts)
        .default(effort_index)
        .interact()
        .map_err(prompt_error)?;
    config.llm.reasoning_effort = bilisum::config::REASONING_EFFORTS[effort].to_string();

    // STT 模型
    let labels: Vec<String> = stt_models::STT_MODELS
        .iter()
        .map(|model| {
            let installed = paths::model_file(model.id)
                .map(|path| path.is_file())
                .unwrap_or(false);
            format!(
                "{:<10} {:>7}  {}{}",
                model.label,
                model.size,
                model.description,
                if installed { "  [已下载]" } else { "" }
            )
        })
        .collect();
    let default_index = stt_models::STT_MODELS
        .iter()
        .position(|model| model.id == config.stt.model)
        .unwrap_or(1);
    let picked = Select::with_theme(&theme)
        .with_prompt("STT 模型")
        .items(&labels)
        .default(default_index)
        .interact()
        .map_err(prompt_error)?;
    config.stt.model = stt_models::STT_MODELS[picked].id.to_string();

    // 语言
    let languages = ["zh-cn（中文）", "en（英文）"];
    let language_index = if config.stt.language == "en" { 1 } else { 0 };
    let language = Select::with_theme(&theme)
        .with_prompt("转写语言")
        .items(languages)
        .default(language_index)
        .interact()
        .map_err(prompt_error)?;
    config.stt.language = if language == 1 { "en" } else { "zh-cn" }.to_string();

    // 字幕来源
    let sources = [
        "audio（音频转写，质量最佳，默认）",
        "subtitle（优先网站字幕，缺失回退转写）",
    ];
    let source_index = if config.stt.source == "subtitle" { 1 } else { 0 };
    let source = Select::with_theme(&theme)
        .with_prompt("字幕来源")
        .items(sources)
        .default(source_index)
        .interact()
        .map_err(prompt_error)?;
    config.stt.source = if source == 1 { "subtitle" } else { "audio" }.to_string();

    // 默认模式
    let modes: Vec<String> = Mode::ALL
        .iter()
        .map(|mode| format!("{:<10} {}", mode.as_str(), mode.label()))
        .collect();
    let mode_index = Mode::ALL
        .iter()
        .position(|mode| *mode == config.default_mode())
        .unwrap_or(0);
    let mode = Select::with_theme(&theme)
        .with_prompt("默认输出模式")
        .items(&modes)
        .default(mode_index)
        .interact()
        .map_err(prompt_error)?;
    config.output.default_mode = Mode::ALL[mode].as_str().to_string();

    // 缓存上限
    let max_size: String = Input::with_theme(&theme)
        .with_prompt("音频缓存上限（GB）")
        .with_initial_text(config.cache.max_size_gb.to_string())
        .allow_empty(false)
        .interact_text()
        .map_err(prompt_error)?;
    config.cache.max_size_gb = max_size.trim().parse().map_err(|_| {
        AppError::user(format!("无效的数值: {max_size}")).with_hint("示例：5")
    })?;

    config.save()?;
    let path = paths::config_file().map_err(|err| AppError::system(err.to_string()))?;
    println!("\n✓ 配置已保存：{}", path.display());
    if !config.has_api_key() {
        println!("  提示：未配置 API Key，仅能生成原始字幕（--mode transcript）");
    }
    let model_path = paths::model_file(&config.stt.model)
        .map_err(|err| AppError::system(err.to_string()))?;
    if model_path.is_file() {
        println!("\n当前转写模型已存在：{}", model_path.display());
    } else {
        println!("\n当前转写模型尚未下载");
    }
    println!("下一步命令：");
    println!(
        "  bilisum models download {} --mirror hf-mirror",
        config.stt.model
    );
    println!("  或运行 bilisum models，交互选择模型和下载镜像");
    println!("  下载完成后运行：bilisum \"<视频链接>\"");
    Ok(())
}

/// 用 $EDITOR 编辑 custom.prompt。
fn edit_custom_prompt() -> Result<()> {
    let mut config = app::bootstrap()?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let tmp = std::env::temp_dir().join(format!("bilisum-prompt-{}.txt", std::process::id()));
    let initial = if config.custom.prompt.trim().is_empty() {
        LEGACY_PROMPT.to_string()
    } else {
        config.custom.prompt.clone()
    };
    std::fs::write(&tmp, initial)
        .map_err(|err| AppError::system(format!("写入临时文件失败：{err}")))?;

    let status = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .map_err(|err| AppError::system(format!("启动 {editor} 失败：{err}")))?;
    if !status.success() {
        return Err(AppError::user(format!("{editor} 退出异常")));
    }
    let content = std::fs::read_to_string(&tmp)
        .map_err(|err| AppError::system(format!("读取临时文件失败：{err}")))?;
    std::fs::remove_file(&tmp).ok();
    config.custom.prompt = content;
    config.save()?;
    println!("✓ custom.prompt 已更新（{} 字符）", config.custom.prompt.chars().count());
    Ok(())
}

fn sessions_ls() -> Result<()> {
    let sessions = session::list()?;
    if sessions.is_empty() {
        println!("还没有任何 session");
        return Ok(());
    }
    println!("{:<32} {:<20} {:<10} 时间", "ID", "标题", "模式");
    for session in sessions {
        let llm = if session.llm_path().is_file() { "✓" } else { "-" };
        println!(
            "{:<32} {:<20} {:<10} {}  llm:{}",
            session.id,
            truncate(&session.meta.title, 18),
            session.meta.mode,
            format_local_time(session.meta.created_at),
            llm
        );
    }
    Ok(())
}

fn sessions_show(id: Option<&str>) -> Result<()> {
    let session = match id {
        Some(id) => Session::open(id)?,
        None => Session::latest()?,
    };
    println!("session  {}", session.id);
    println!("标题     {}", session.meta.title);
    println!("链接     {}", session.meta.url);
    println!("平台     {}", session.meta.platform.label());
    println!("来源     {}", session.meta.source.label());
    println!("模式     {}", session.meta.mode);
    if !session.meta.model.is_empty() {
        println!("模型     {}", session.meta.model);
    }
    println!("时间     {}", format_local_time(session.meta.created_at));
    if !session.meta.audio_cache_key.is_empty() {
        println!("缓存键   {}", session.meta.audio_cache_key);
    }
    println!("目录     {}", session.dir.display());
    println!("产物：");
    for (name, exists) in session.artifacts() {
        println!("  {} {}", if exists { "✓" } else { "·" }, name);
    }
    Ok(())
}

fn cache_ls() -> Result<()> {
    let entries = bilisum::cache::list()?;
    if entries.is_empty() {
        println!("缓存为空");
        return Ok(());
    }
    for entry in &entries {
        println!(
            "{:<28} {:>10}  {}",
            entry.key,
            paths::human_size(entry.size),
            bilisum::cache::modified_label(entry)
        );
    }
    println!(
        "\n共 {} 个文件，占用 {}",
        entries.len(),
        paths::human_size(bilisum::cache::total_size()?)
    );
    Ok(())
}

fn validate_cache_key(video_key: &str) -> Result<()> {
    if video_key.is_empty()
        || video_key.contains('/')
        || video_key.contains('\\')
        || video_key.contains("..")
    {
        return Err(AppError::user(format!("非法缓存键: {video_key}")));
    }
    Ok(())
}

fn cache_size() -> Result<()> {
    let config = app::bootstrap()?;
    let size = bilisum::cache::total_size()?;
    let limit = (config.cache.max_size_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
    println!("当前占用：{}", paths::human_size(size));
    if limit == 0 {
        println!("缓存上限：不限");
    } else {
        println!("缓存上限：{}", paths::human_size(limit));
        println!("使用比例：{:.1}%", size as f64 / limit as f64 * 100.0);
    }
    Ok(())
}

fn cache_path(video_key: &str) -> Result<()> {
    validate_cache_key(video_key)?;
    let path = paths::audio_cache_file(video_key)
        .map_err(|err| AppError::system(err.to_string()))?;
    println!("{}", path.display());
    if !path.is_file() {
        eprintln!("提示：缓存不存在");
    }
    Ok(())
}

fn cache_rm(video_key: &str) -> Result<()> {
    validate_cache_key(video_key)?;
    if bilisum::cache::remove(video_key)? {
        println!("✓ 已删除缓存：{video_key}");
        Ok(())
    } else {
        Err(AppError::user(format!("缓存不存在: {video_key}"))
            .with_hint("查看缓存：bilisum cache ls"))
    }
}

fn cache_prune() -> Result<()> {
    let config = app::bootstrap()?;
    let max_bytes = (config.cache.max_size_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
    if max_bytes == 0 {
        println!("缓存上限未启用，无需清理");
        return Ok(());
    }
    let removed = bilisum::cache::enforce_limit(max_bytes)?;
    println!("✓ 已按 LRU 清理 {removed} 个缓存文件");
    println!("当前占用：{}", paths::human_size(bilisum::cache::total_size()?));
    Ok(())
}

/// 交互式选择 STT 模型；返回 (模型 id, 是否已下载)。
fn choose_model() -> Result<(String, bool)> {
    let theme = ColorfulTheme::default();
    let labels: Vec<String> = stt_models::STT_MODELS
        .iter()
        .map(|model| {
            let installed = paths::model_file(model.id)
                .map(|path| path.is_file())
                .unwrap_or(false);
            format!(
                "{:<10} {:>7}  {}{}",
                model.label,
                model.size,
                model.description,
                if installed { "  [已下载]" } else { "" }
            )
        })
        .collect();
    let default_index = stt_models::STT_MODELS
        .iter()
        .position(|model| model.id == stt_models::DEFAULT_STT_MODEL)
        .unwrap_or(1);
    let picked = Select::with_theme(&theme)
        .with_prompt("选择 STT 模型")
        .items(&labels)
        .default(default_index)
        .interact()
        .map_err(prompt_error)?;
    let id = stt_models::STT_MODELS[picked].id.to_string();

    let already = paths::model_file(&id)
        .map(|path| path.is_file())
        .unwrap_or(false);
    Ok((id, already))
}

/// 校验模型 id（供下载前早失败）。
fn validate_model_id(id: &str) -> Result<()> {
    stt_models::find(id)
        .map(|_| ())
        .ok_or_else(|| {
            AppError::user(format!("未知的 STT 模型: {id}"))
                .with_hint(format!("可用：{}", stt_models::ids().join(", ")))
        })
}

/// 交互式选择下载镜像（默认国内镜像）。
fn choose_mirror() -> Result<stt_models::Mirror> {
    let theme = ColorfulTheme::default();
    let labels: Vec<&str> = stt_models::Mirror::ALL.iter().map(|m| m.label()).collect();
    // 默认 hf-mirror：国内网络下官方域名常不可达
    let default_index = stt_models::Mirror::ALL
        .iter()
        .position(|m| *m == stt_models::Mirror::HfMirror)
        .unwrap_or(0);
    let picked = Select::with_theme(&theme)
        .with_prompt("下载镜像")
        .items(&labels)
        .default(default_index)
        .interact()
        .map_err(prompt_error)?;
    Ok(stt_models::Mirror::ALL[picked])
}

fn prompt_error(err: dialoguer::Error) -> AppError {
    AppError::user(format!("交互输入失败：{err}"))
}

fn redact(key: &str, value: &str) -> String {
    if key.contains("api_key") || key.contains("cookie") {
        if value.chars().count() <= 8 {
            "****".to_string()
        } else {
            format!("{}****", value.chars().take(4).collect::<String>())
        }
    } else {
        value.to_string()
    }
}

/// 未使用的导入保护（`AnyResult` 供未来扩展）。
#[allow(dead_code)]
fn _unused(_: AnyResult<()>) {}

/// `Confirm` 在 settings 流程里按需使用；保留导入以免将来删除。
#[allow(dead_code)]
fn _confirm(theme: &ColorfulTheme) -> AnyResult<bool> {
    Confirm::with_theme(theme)
        .with_prompt("继续？")
        .interact()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

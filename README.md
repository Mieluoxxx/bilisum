# bilisum

B站 / YouTube 视频字幕与摘要 CLI。Rust 单文件二进制，无 GUI、无运行时依赖。

- **两阶段流水线**：先抓字幕（零 LLM 成本），再按模式生成产物
- **可续跑**：`-c` 复用已有字幕，换模式重生成不必重新转写
- **本地转写**：yt-dlp + ffmpeg + whisper-rs（Metal 加速），音频走全局缓存
- **纯终端**：进度写 stderr，产物写 stdout，可管道

## 前置要求

```bash
# 运行时依赖（必需）
brew install yt-dlp ffmpeg

# 编译依赖（仅自行构建时需要）
brew install cmake

# STT 模型（选模型 → 选镜像 → 自动下载）
bilisum models
# 或非交互：bilisum models download <model-file> --mirror hf-mirror
```

> `whisper-rs` 需要 `cmake` 编译 whisper.cpp；Apple Silicon 自动启用 Metal 加速。
> 模型下载支持镜像选择：`HuggingFace 官方`（海外）或 `hf-mirror.com`（国内，默认）。
> 选中的镜像失败会自动回退另一个。

自检：`bilisum doctor` 逐项检查上述全部依赖并给出修复命令。

## 安装

```bash
cargo install --path .
# 或
cargo build --release && cp target/release/bilisum ~/.local/bin/
```

## 快速开始

```bash
bilisum settings                    # 配置向导：API Key / 模型 / STT
bilisum login                       # B站扫码登录（终端二维码，可选）
bilisum "https://www.bilibili.com/video/BV1xx411c7mD"
```

## 命令

```
bilisum <url>                        # 默认模式（polish）
bilisum <url> --mode <mode>          # 指定模式
bilisum <url> --source <source>      # audio（默认）| subtitle
bilisum <url> -o out.md              # 落文件（省略则写 stdout）
bilisum <url> --json                 # 机器可读输出
bilisum -c [id]                      # 续跑最近一次 / 指定 session
bilisum -c [id] -f                   # 丢弃 raw.txt 重新抓取（复用音频缓存）
bilisum -c [id] --no-cache           # 连音频也重新下载

bilisum settings                     # 问答式配置
bilisum settings set <key> <value>   # 单键修改
bilisum settings edit                # $EDITOR 编辑 custom.prompt
bilisum settings test                # 测试大模型连通性

bilisum login                        # B站扫码登录
bilisum whoami                       # 登录态（已登录/未登录/已过期/服务异常）
bilisum models                       # 选模型 → 选镜像 → 自动下载
bilisum models download <id> [--mirror huggingface|hf-mirror]
bilisum sessions [ls|show|rm] [id]   # 历史任务
bilisum view ls                       # 列出已缓存内容
bilisum view <url-or-bv>              # URL 或 BV号，查看对应 llm.txt/raw.txt
bilisum cache ls                      # 列出音频缓存
bilisum cache size                    # 查看缓存占用与上限
bilisum cache path <video-key>        # 查看指定视频缓存路径
bilisum cache rm <video-key>          # 删除单个缓存
bilisum cache prune                   # 按 LRU 上限清理
bilisum cache clean                   # 清空全部缓存
bilisum doctor                       # 环境自检
```

退出码：`0` 成功 / `1` 用户错误（参数、缺 key、链接无效）/ `2` 系统错误（网络、子进程、缺依赖）。

## 模式

| 模式 | 调 LLM | 说明 |
| --- | --- | --- |
| `polish` | 是（默认） | 整段发送给大模型润色，修同音字/专有名词/口误，输出纯文本无时间戳 |
| `transcript` | 否 | 直接输出原始字幕，零成本逃生舱 |
| `timestamp` | 是 | 同 polish 校对 + 15s 合并 + `[mm:ss-mm:ss]` 渲染 |
| `summary` | 是 | 主题级抽象摘要（核心论点/关键要点/启示） |
| `fulltext` | 是 | 改写为流畅文章（按话题分 H3） |
| `custom` | 是 | 使用 `custom.prompt` 模板（`{{title}}` / `{{transcript}}` 占位） |

改默认模式：`bilisum settings set output.default_mode timestamp`

`timestamp` 走**分块 1:1 校对**（10 行/块 × 3 并发，带行号回填）；`polish` 直接整段发送，保留完整上下文。

## 数据流

```
bilisum <url>                       bilisum -c [id] --mode summary
   │                                   │
   ├─ 阶段 A prepare（零 LLM 成本）      └─ 直接读已有 raw.txt / segments.json
   │   ├─ 抓标题（B站 view / YT oEmbed）
   │   ├─ --source audio（默认）→ yt-dlp → ffmpeg → whisper-rs
   │   │   --source subtitle     → 网站字幕（缺失自动回退转写）
   │   ├─ 音频走全局缓存 ~/.bili/cache/{video_key}.wav
   │   └─ 落 raw.txt + segments.json
   │
   └─ 阶段 B generate（调 LLM）
       ├─ 无 API Key → 阶段 A 已完成，raw.txt 保留，退出码 1
       └─ 落 llm.txt
```

**关键**：阶段 A 的产物是资产。没有 API Key 也能拿到字幕；配好 key 后 `bilisum -c` 直接续跑，不必重新下载和转写。

## 目录布局

```
~/.bili/
├── config.toml              # [llm] [stt] [output] [custom] [cache]
├── sessions/<id>/           # id = BV号 或 youtube-<video_id>
│   ├── meta.json            # url/title/platform/source/mode/model/created_at/audio_cache_key
│   ├── raw.txt              # 原始字幕，纯文本无时间戳，永不覆盖
│   ├── segments.json        # 分段结构（timestamp 模式的唯一数据源）
│   └── llm.txt              # LLM 产物，单文件覆盖
├── cache/{video_key}.wav    # 音频缓存，LRU 上限默认 5GB
├── models/ggml-*.bin        # Whisper 模型
├── logs/                    # tracing 日志（--log 时写入）
├── cookies-bilibili.txt     # bilisum login 写入
└── cookies-youtube.txt      # 用户自备（login 不覆盖）
```

每次执行结束会打印 session ID 与重新生成命令：

```
✓ 完成  标题 · 摘要
  session  BV1pXhu62EHL
  raw      ~/.bili/sessions/.../raw.txt
  llm      ~/.bili/sessions/.../llm.txt
  重新生成 bilisum -c BV1pXhu62EHL --mode fulltext
```

## 配置项

```
bilisum settings set llm.api_key sk-xxx      # 必填（非 transcript 模式）
bilisum settings set llm.base_url https://api.deepseek.com/v1
bilisum settings set llm.model deepseek-chat
bilisum settings set llm.reasoning_effort none   # 推理强度：none(默认)|minimal|low|medium|high
bilisum settings set stt.model <model-file>
bilisum settings set stt.language zh-cn      # zh-cn | en
bilisum settings set stt.source audio        # audio | subtitle
bilisum settings set output.default_mode polish
bilisum settings set cache.max_size_gb 10
```

> **推理模型提示**：DeepSeek reasoner 等模型的 `reasoning_content` 会先占用输出预算。
> `reasoning_effort = none` 关闭推理后，同一份 47 行字幕的校对从 ~15s 降到 ~4s。
> 不支持该参数的端点会忽略它；留空则不下发。

STT 模型：`ggml-tiny.bin`(75MB) / `ggml-base.bin`(148MB, 默认) / `ggml-small.bin`(488MB) / `ggml-medium.bin`(1.5GB) / `ggml-large-v3.bin`(3.1GB)。

## 常见问题

**B站字幕抓不到**：B站字幕接口需要登录态。`bilisum login` 后重试，或直接走默认的音频转写。

**YouTube 下载失败**：需要 cookie。导出 Netscape 格式的 cookies.txt 放到 `~/.bili/cookies-youtube.txt`。

**模型下载慢/失败**：`bilisum models` 时选 `hf-mirror.com`（国内可达，实测 ~6 MB/s）；
两个镜像都会失败时可手动下载到 `~/.bili/models/`。

**想省磁盘**：`bilisum cache ls` 看占用，`bilisum cache clean` 清空；或调小 `cache.max_size_gb`。

## 开发

```bash
cargo test                    # 全部测试
cargo test -- --nocapture     # 含 whisper 真实推理（需 BILISUM_TEST_WAV）
cargo check --all-targets     # 编译检查
```

代码分层：

- `app.rs` — 两阶段流水线编排（不打印，只返回结果 + 进度回调，为将来 TUI 留边界）
- `whisper.rs` / `subtitle.rs` / `llm.rs` — 外部依赖
- `session.rs` / `cache.rs` / `config.rs` / `paths.rs` — 持久化
- `auth.rs` / `models.rs` / `doctor.rs` — 运维命令
- `main.rs` — 仅参数解析与 I/O

## 许可

MIT

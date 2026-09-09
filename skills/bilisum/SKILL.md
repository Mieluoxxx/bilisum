---
name: bilisum
description: 使用 bilisum CLI 完成 B站/YouTube 视频转写、LLM 生成、模型管理、session 续跑和故障排查。
---

# bilisum

使用本仓库的 `bilisum` CLI 处理 B站/YouTube 视频。

## 执行规则

1. 先检查命令：`command -v bilisum`；异常时运行 `bilisum doctor`。
2. 默认使用 `audio` 音频转写；除非用户明确要求，不使用 `--source subtitle`。
3. 不要擅自使用 `-f/--force`；它会覆盖同视频 session 的 `raw.txt` 并清掉旧 `llm.txt`。
4. 已有字幕时优先使用 `bilisum -c <session-id>`，不要重新转写。
5. 脚本消费结果时使用 `--json`；产物在 stdout，进度和诊断在 stderr。
6. 禁止在输出、日志或回复中暴露 API Key、Cookie、完整授权 URL。

## 初始化

```bash
brew install yt-dlp ffmpeg cmake
bilisum settings
bilisum models
bilisum doctor
```

`bilisum models` 会依次选择 STT 模型、选择下载镜像，然后自动下载。国内网络选择
`hf-mirror.com（国内镜像，推荐）`。

非交互下载：

```bash
bilisum models download <model-file> --mirror hf-mirror
```

## 视频任务

```bash
bilisum "<视频 URL>"                    # 默认 polish：整段字幕一次发送给 LLM
bilisum "<视频 URL>" --mode transcript  # 只转写，不调用 LLM
bilisum "<视频 URL>" --mode summary
bilisum "<视频 URL>" --mode fulltext
bilisum "<视频 URL>" --mode timestamp
bilisum "<视频 URL>" -o <output-file>
```

默认 `polish` 会直接读取无时间戳的 `raw.txt`，整段发送给大模型，返回连续纯文本。
`timestamp` 为保持时间段对齐，才使用分块校对。

## LLM 配置

```bash
bilisum settings
bilisum settings set llm.api_key '<API_KEY>'
bilisum settings set llm.base_url 'https://api.deepseek.com/v1'
bilisum settings set llm.model '<LLM_MODEL>'
bilisum settings set llm.reasoning_effort none
bilisum settings test
```

`reasoning_effort=none` 适合字幕润色，可避免推理模型浪费时间和输出预算。

## session 续跑

每个视频对应一个 session：B站使用 BV 号，YouTube 使用 `youtube-<video-id>`。

```bash
bilisum sessions ls
bilisum -c <session-id> --mode summary
bilisum -c <session-id> --mode fulltext
bilisum -c --mode summary                 # 最近一次
bilisum -c <session-id> -f                 # 强制重新转写
bilisum -c <session-id> -f --no-cache      # 连音频缓存也丢弃
bilisum view ls                            # 列出已缓存内容
bilisum view "<视频 URL 或 BV号>"            # 查看对应视频缓存
bilisum cache ls                            # 列出音频缓存
bilisum cache size                          # 查看缓存占用与上限
bilisum cache path <video-key>              # 获取缓存路径
bilisum cache rm <video-key>                # 删除单个缓存
bilisum cache prune                         # 按 LRU 上限清理
bilisum cache clean                         # 清空全部缓存
```

阶段 A 会保存：

```text
~/.bili/sessions/<session-id>/raw.txt
~/.bili/sessions/<session-id>/segments.json
~/.bili/sessions/<session-id>/llm.txt
```

LLM 配置失败时不要重新转写；修好配置后直接使用 `-c <session-id>`。

## 登录与诊断

```bash
bilisum login
bilisum whoami
bilisum doctor
bilisum -vv -c <session-id> --mode transcript --log
```

B站和 YouTube Cookie 分开保存：

```text
~/.bili/cookies-bilibili.txt
~/.bili/cookies-youtube.txt
```

## 辅助脚本

脚本位于 `skills/bilisum/scripts/`：

```bash
skills/bilisum/scripts/doctor.sh
skills/bilisum/scripts/model.sh <model-file> [huggingface|hf-mirror]
skills/bilisum/scripts/run.sh <video-url> [mode]
skills/bilisum/scripts/continue.sh [session-id] [mode]
skills/bilisum/scripts/cache.sh [ls|size|path|rm|prune|clean] [args]
```

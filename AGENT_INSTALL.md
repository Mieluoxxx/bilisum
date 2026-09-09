# bilisum Agent 安装指南

## 安装 CLI

在 macOS 上安装运行依赖：

```bash
brew install yt-dlp ffmpeg cmake
```

在仓库根目录安装 `bilisum`：

```bash
cargo install --path . --force
```

确认安装：

```bash
command -v bilisum
bilisum --version
bilisum doctor
```

## 安装 Agent Skill

本仓库唯一主 skill 位于：

```text
skills/bilisum/SKILL.md
```

支持 skills 目录的 Agent 可将 `skills/bilisum/` 复制或链接到自己的 skills 目录：

```bash
cp -R skills/bilisum ~/.agents/skills/bilisum
# 或使用软链接，便于跟随仓库更新
ln -sfn "$PWD/skills/bilisum" ~/.agents/skills/bilisum
```

Agent 加载 `skills/bilisum/SKILL.md` 后，根据任务调用 `bilisum` CLI；辅助脚本位于
`skills/bilisum/scripts/`。

## 首次初始化

```bash
bilisum settings
bilisum models
bilisum login       # B站需要登录时执行
bilisum doctor
```

`bilisum models` 会先选择模型，再选择 HuggingFace 官方或
`hf-mirror.com` 国内镜像，最后自动下载。模型保存到 `~/.bili/models/`。

缓存管理：

```bash
bilisum cache ls
bilisum cache size
bilisum cache path <video-key>
bilisum cache rm <video-key>
bilisum cache prune
bilisum cache clean
```

Agent 也可以调用 `skills/bilisum/scripts/cache.sh` 统一转发这些命令。

## Agent 安全规则

- 不要输出或记录 API Key、Cookie、完整登录 URL。
- 不要未经用户确认使用 `-f/--force`。
- 优先复用 `-c <session-id>`，避免重复下载和转写。
- 脚本需要结构化结果时使用 `--json`。

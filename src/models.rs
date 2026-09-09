//! STT 模型下载（HuggingFace 流式 + 进度条）。

use futures_util::StreamExt;

use crate::app::ProgressEvent;
use crate::error::{AppError, IntoAppResult, Result};
use crate::paths;
use crate::stt_models;

/// 下载指定模型到 `~/.bili/models/`。
///
/// `mirror` 指定首选镜像；失败时自动回退另一个（网络环境差异大）。
pub async fn download(
    id: &str,
    mirror: stt_models::Mirror,
    progress: &(dyn Fn(ProgressEvent<'_>) + Send + Sync),
) -> Result<()> {
    let model = stt_models::find(id).ok_or_else(|| {
        AppError::user(format!("未知的 STT 模型: {id}"))
            .with_hint(format!("可用：{}", stt_models::ids().join(", ")))
    })?;
    if !stt_models::is_valid_id(id) {
        return Err(AppError::user(format!("非法模型名: {id}")));
    }
    paths::ensure_layout().map_err(|err| AppError::system(err.to_string()))?;

    let target = paths::model_file(id).map_err(|err| AppError::system(err.to_string()))?;
    if target.is_file() {
        println!("✓ {} 已存在：{}", model.label, target.display());
        return Ok(());
    }

    progress(ProgressEvent {
        stage: "download",
        detail: &format!("下载 {}（{}）", model.label, model.size),
    });

    // 用 read_timeout（空闲超时）而不是 timeout（总超时）：
    // 实测总超时会显著拖慢大文件下载（1.5 MB/s → 4 MB/s）。
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|err| AppError::system(format!("初始化下载客户端失败：{err}")))?;

    // 选中的镜像优先，失败自动回退另一个
    let mut mirrors = vec![mirror];
    mirrors.extend(
        stt_models::Mirror::ALL
            .into_iter()
            .filter(|other| *other != mirror),
    );

    let tmp = target.with_extension("bin.part");
    // 断点续传：.part 已存在则从断点继续（镜像偶尔中断，148MB+ 文件重下代价高）
    let mut written: u64 = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let bar = indicatif::ProgressBar::new(model.size_bytes);
    bar.set_position(written);
    bar.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner} {bar:32} {bytes}/{total_bytes} ({eta})",
        )
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
    );

    let mut last_error = String::new();
    let mut finished = false;
    // 每个镜像内再重试：网络抖动是常态
    'outer: for mirror in mirrors {
        let url = stt_models::download_url(id, mirror);
        for attempt in 1..=MAX_ATTEMPTS {
            if written > 0 {
                progress(ProgressEvent {
                    stage: "download",
                    detail: &format!("从 {written} 字节续传（尝试 {attempt}）"),
                });
            }
            let mut request = client.get(&url);
            if written > 0 {
                request = request.header("Range", format!("bytes={written}-"));
            }
            let resp = match request.send().await {
                Ok(resp) => resp,
                Err(err) => {
                    last_error = format!("{err} ({url})");
                    progress(ProgressEvent {
                        stage: "download",
                        detail: &format!("连接失败，重试：{last_error}"),
                    });
                    continue;
                }
            };
            // 206 = 断点续传成功；200 = 服务端不支持 Range，需从头写
            let status = resp.status();
            if status.as_u16() == 200 && written > 0 {
                written = 0;
                let _ = std::fs::remove_file(&tmp);
                bar.set_position(0);
            } else if !status.is_success() {
                last_error = format!("HTTP {status} ({url})");
                progress(ProgressEvent {
                    stage: "download",
                    detail: &format!("镜像不可用，尝试下一个：{last_error}"),
                });
                continue 'outer;
            }

            let mut file = if written > 0 {
                tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(&tmp)
                    .await
            } else {
                tokio::fs::File::create(&tmp).await
            }
            .sys_context(format!("打开文件失败: {}", tmp.display()))?;

            let mut stream = resp.bytes_stream();
            let mut broken = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => {
                        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                            .await
                            .sys_context("写入模型文件失败")?;
                        written += chunk.len() as u64;
                        bar.set_position(written);
                    }
                    Err(err) => {
                        broken = Some(err.to_string());
                        break;
                    }
                }
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .sys_context("刷新模型文件失败")?;
            drop(file);

            if broken.is_none() {
                finished = true;
                break 'outer;
            }
            last_error = broken.unwrap_or_default();
            progress(ProgressEvent {
                stage: "download",
                detail: &format!("传输中断，续传：{last_error}"),
            });
        }
    }
    bar.finish_and_clear();

    if !finished {
        return Err(AppError::system(format!("下载失败：{last_error}"))
            .with_hint(format!(
                "已下载 {written} 字节，可重跑同命令续传；或手动下载到 {}",
                target.display()
            )));
    }
    // 校验完整性（服务端给了长度时）
    if written < model.size_bytes / 2 {
        return Err(AppError::system(format!(
            "下载内容明显偏小：{written} 字节（预期约 {}）",
            model.size_bytes
        ))
        .with_hint(format!("删除后重试：rm {}", tmp.display())));
    }
    std::fs::rename(&tmp, &target).sys_context(format!(
        "保存模型失败: {}",
        target.display()
    ))?;
    tracing::info!(model = id, bytes = written, path = %target.display(), "模型下载完成");
    println!("✓ 已下载 {}（{}）→ {}", model.label, model.size, target.display());
    Ok(())
}

/// 单个镜像内的最大重试次数。
const MAX_ATTEMPTS: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_model_is_user_error() {
        let err = download("ggml-nope.bin", stt_models::Mirror::HfMirror, &|_| {}).await.unwrap_err();
        assert_eq!(err.class, crate::error::ExitClass::User);
    }

    #[test]
    fn existing_model_short_circuits() {
        // 已存在时必须短路，不发网络请求。
        // 全程持有隔离锁（同步测试，无 await），避免与其他测试竞争 BILISUM_HOME。
        crate::test_support::with_temp_home("models", || {
            paths::ensure_layout().unwrap();
            let target = paths::model_file("ggml-tiny.bin").unwrap();
            std::fs::write(&target, b"fake").unwrap();

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime
                .block_on(download("ggml-tiny.bin", stt_models::Mirror::HfMirror, &|_| {}))
                .expect("已存在的模型应短路成功");

            assert!(target.is_file(), "已存在的模型不应被删除");
        });
    }
}

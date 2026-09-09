//! Whisper STT 模型清单（ggml 格式，`~/.bili/models/`）。

/// 单个模型条目。
pub struct SttModel {
    /// 文件名（也是 config 中的取值）。
    pub id: &'static str,
    /// 展示名。
    pub label: &'static str,
    /// 人类可读大小。
    pub size: &'static str,
    /// 字节数（用于下载进度与占用统计）。
    pub size_bytes: u64,
    /// 简短描述。
    pub description: &'static str,
}

/// 五档模型，与 GUI 版一致。
pub const STT_MODELS: &[SttModel] = &[
    SttModel {
        id: "ggml-tiny.bin",
        label: "tiny",
        size: "75 MB",
        size_bytes: 75 * 1024 * 1024,
        description: "最快，精度较低",
    },
    SttModel {
        id: "ggml-base.bin",
        label: "base",
        size: "148 MB",
        size_bytes: 148 * 1024 * 1024,
        description: "默认，速度与精度均衡",
    },
    SttModel {
        id: "ggml-small.bin",
        label: "small",
        size: "488 MB",
        size_bytes: 488 * 1024 * 1024,
        description: "较高精度",
    },
    SttModel {
        id: "ggml-medium.bin",
        label: "medium",
        size: "1.5 GB",
        size_bytes: 1536 * 1024 * 1024,
        description: "高精度，较慢",
    },
    SttModel {
        id: "ggml-large-v3.bin",
        label: "large-v3",
        size: "3.1 GB",
        size_bytes: 3180 * 1024 * 1024,
        description: "最高精度，最慢",
    },
];

/// 默认模型。
pub const DEFAULT_STT_MODEL: &str = "ggml-base.bin";

/// 按 id 查找。
pub fn find(id: &str) -> Option<&'static SttModel> {
    STT_MODELS.iter().find(|model| model.id == id)
}

/// 全部 id（错误提示用）。
pub fn ids() -> Vec<&'static str> {
    STT_MODELS.iter().map(|model| model.id).collect()
}

/// 模型下载地址候选列表（按顺序尝试）。
///
/// HuggingFace 官方域名在部分网络不可达，`hf-mirror.com` 是社区维护的只读镜像。
pub fn download_urls(id: &str) -> Vec<String> {
    vec![download_url(id, Mirror::HuggingFace), download_url(id, Mirror::HfMirror)]
}

/// 模型下载镜像。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    /// HuggingFace 官方（海外网络快）。
    HuggingFace,
    /// hf-mirror.com（社区维护的 HuggingFace 只读镜像，国内可达）。
    HfMirror,
}

impl Mirror {
    pub const ALL: [Mirror; 2] = [Mirror::HuggingFace, Mirror::HfMirror];

    /// 配置/命令行取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Mirror::HuggingFace => "huggingface",
            Mirror::HfMirror => "hf-mirror",
        }
    }

    /// 交互菜单展示名。
    pub fn label(self) -> &'static str {
        match self {
            Mirror::HuggingFace => "HuggingFace 官方（海外网络）",
            Mirror::HfMirror => "hf-mirror.com（国内镜像，推荐）",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "huggingface" | "hf" | "official" => Some(Mirror::HuggingFace),
            "hf-mirror" | "mirror" | "hf_mirror" => Some(Mirror::HfMirror),
            _ => None,
        }
    }

    /// 仓库根地址。
    pub fn base_url(self) -> &'static str {
        match self {
            Mirror::HuggingFace => "https://huggingface.co",
            Mirror::HfMirror => "https://hf-mirror.com",
        }
    }
}

/// 指定镜像下的模型地址。
pub fn download_url(id: &str, mirror: Mirror) -> String {
    format!(
        "{}/ggerganov/whisper.cpp/resolve/main/{id}",
        mirror.base_url()
    )
}

/// 文件名是否合法（防路径注入）。
pub fn is_valid_id(id: &str) -> bool {
    id.starts_with("ggml-") && id.ends_with(".bin") && !id.contains('/') && !id.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_exists_in_catalog() {
        assert!(find(DEFAULT_STT_MODEL).is_some());
    }

    #[test]
    fn every_catalog_entry_is_valid_id() {
        for model in STT_MODELS {
            assert!(is_valid_id(model.id), "非法 id: {}", model.id);
            for mirror in Mirror::ALL {
                let url = download_url(model.id, mirror);
                assert!(url.contains(model.id));
                assert!(url.starts_with(mirror.base_url()));
            }
        }
    }

    #[test]
    fn mirror_roundtrips_and_rejects_unknown() {
        for mirror in Mirror::ALL {
            assert_eq!(Mirror::parse(mirror.as_str()), Some(mirror));
        }
        assert_eq!(Mirror::parse("mirror"), Some(Mirror::HfMirror));
        assert_eq!(Mirror::parse("nope"), None);
    }

    #[test]
    fn mirror_urls_point_at_different_hosts() {
        let hf = download_url("ggml-base.bin", Mirror::HuggingFace);
        let mirror = download_url("ggml-base.bin", Mirror::HfMirror);
        assert!(hf.starts_with("https://huggingface.co/"));
        assert!(mirror.starts_with("https://hf-mirror.com/"));
        assert!(hf.ends_with("/ggml-base.bin") && mirror.ends_with("/ggml-base.bin"));
    }

    #[test]
    fn path_traversal_rejected() {
        assert!(!is_valid_id("../../etc/passwd"));
        assert!(!is_valid_id("ggml-base.bin/../x"));
        assert!(!is_valid_id("model.bin"));
    }
}

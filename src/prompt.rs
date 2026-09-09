//! 内置 prompt 模板。
//!
//! 模板中的 `{{title}}` / `{{transcript}}` 为占位符，渲染时整体替换。

/// 主题级抽象摘要。
pub const SUMMARY_PROMPT: &str = r#"你是视频内容摘要助手。请基于以下视频标题与字幕，输出一份结构化摘要。

标题：{{title}}

字幕：
{{transcript}}

输出（Markdown 格式）：

## 核心论点

一句话（40-60 字）概括视频最核心的观点或结论。

## 关键要点

3-5 条 bullet，列出视频中最重要的论点、证据或结论。每条 1-2 句，不要简单重复字幕原文。

## 启示

一段 100-200 字的反思，说明这些要点为何重要、对读者意味着什么。

要求：
- 全部用简体中文
- 不要逐句重述字幕
- 不要带时间戳
- 不要编造字幕里不存在的内容"#;

/// 流畅文章改写。
pub const FULLTEXT_PROMPT: &str = r#"你是字幕改写助手。请把以下视频字幕改写成一篇流畅可读的文章。

标题：{{title}}

原始字幕：
{{transcript}}

改写要求：
1. 按话题分 3-6 段，每段加 H3 小标题（###）
2. 用标题和上下文修正专有名词、同音错字、口误
3. 去掉语气词、重复、明显口误
4. 不新增字幕里不存在的事实、观点、例子
5. 不带时间戳
6. 全部转为简体中文
7. 输出 Markdown"#;

/// 1:1 段级校对（时间戳模式用）。
pub const TIMESTAMP_PROMPT: &str = r#"你是字幕校对助手。请对以下字幕做 1:1 段级校对。

标题：{{title}}

字幕（每行一段，严格对齐）：
{{transcript}}

校对要求：
1. 每行输入对应一行输出，**不要合并、拆分、增删行**
2. 利用标题和上下文修正同音错字、专有名词（如 "家底谈" 修正为 "Karpathy"，"爆破大叔" 修正为 "Uncle Bob"）
3. 修正明显口误、错别字、标点
4. 保持原意，不改变语气
5. 全部转为简体中文

输出：每行一段，行数与输入严格一致。不要输出任何解释、行号或前缀。"#;

/// 润色字幕（默认模式）：整段发送，输出无时间戳纯文本。
pub const POLISH_PROMPT: &str = r#"你是字幕润色助手。请把以下视频转写字幕润色成一段流畅、自然、忠实原意的纯文本。

标题：{{title}}

原始转写字幕：
{{transcript}}

润色要求：
1. 保持原意和信息完整，不总结、不编造、不大幅改写
2. 根据标题和上下文修正同音错字、专有名词、明显口误和标点
3. 删除无意义的语气词与重复，但不要删除有效内容
4. 全部转为简体中文
5. 不要输出时间戳、标题、编号或任何解释

输出：直接返回润色后的连续纯文本。"#;

/// Pre-mode 旧默认模板，作为 custom 模式的初始值保留。
pub const LEGACY_PROMPT: &str = r#"你是一位擅长整理视频的助手。请根据以下内容生成结构化总结，并保留关键时间戳。

标题：{{title}}

字幕：
{{transcript}}

输出要求：
- 输出 Markdown
- 先给 1 句高密度摘要（40-60 字）
- 再给 3-5 条要点列表，每条附带时间戳（如 01:23）
- 单独输出"字幕摘录"小节，用项目符号逐条列出需要引用的字幕内容
- 字幕内容必须转换为简体中文（若原字幕为繁体或混杂语言，先转为简体再输出）
- 最后给 1 段 200-400 字的完整总结
- 全部使用简体中文
"#;

/// timestamp 分块校对参数：每块行数与每批并行块数（批间串行，控制瞬时并发）。
pub const TIMESTAMP_CHUNK_SIZE: usize = 10;
pub const TIMESTAMP_BATCH_CONCURRENCY: usize = 3;

/// timestamp 分块校对 prompt：只含本块编号行，避免长字幕一次送入（超时/超 context）。
pub fn build_timestamp_chunk_prompt(title: &str, numbered_lines: &str) -> String {
    format!(
        r#"你是字幕校对助手。请对以下字幕片段做 1:1 逐行校对（这是完整字幕的一部分）。

标题：{title}

字幕（每行已编号，保持对齐）：
{numbered_lines}

校对要求：
1. 每行输入对应一行输出，不要合并、拆分、增删行
2. 利用标题修正同音错字、专有名词；修正口误、错别字、标点
3. 保持原意，不改变语气
4. 全部转为简体中文

输出：每行保持原编号（如 `1. 修正后文本`），行数与输入严格一致。不要解释、标题或额外内容。"#
    )
}

/// 解析指定模式对应的模板；custom 模式用用户模板，空串回退 SUMMARY_PROMPT。
pub fn resolve_prompt(mode: crate::mode::Mode, custom_prompt: &str) -> String {
    use crate::mode::Mode;
    match mode {
        Mode::Polish => POLISH_PROMPT.to_string(),
        Mode::Timestamp => TIMESTAMP_PROMPT.to_string(),
        Mode::Summary => SUMMARY_PROMPT.to_string(),
        Mode::Fulltext => FULLTEXT_PROMPT.to_string(),
        Mode::Custom => {
            let custom = custom_prompt.trim();
            if custom.is_empty() {
                SUMMARY_PROMPT.to_string()
            } else {
                custom.to_string()
            }
        }
        // transcript 不调用 LLM；返回润色模板仅为类型完整（不会被使用）
        Mode::Transcript => POLISH_PROMPT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    #[test]
    fn templates_keep_placeholders() {
        for template in [SUMMARY_PROMPT, FULLTEXT_PROMPT, TIMESTAMP_PROMPT, LEGACY_PROMPT] {
            assert!(template.contains("{{title}}"));
            assert!(template.contains("{{transcript}}"));
        }
    }

    #[test]
    fn polish_prompt_forbids_timestamps() {
        assert!(POLISH_PROMPT.contains("不要输出时间戳"));
        assert!(POLISH_PROMPT.contains("连续纯文本"));
        assert!(!POLISH_PROMPT.contains("每行输入对应一行输出"));
    }

    #[test]
    fn chunk_prompt_embeds_title_and_lines() {
        let prompt = build_timestamp_chunk_prompt("标题B", "2. x");
        assert!(prompt.contains("标题B"));
        assert!(prompt.contains("2. x"));
    }

    #[test]
    fn resolve_prompt_maps_modes() {
        assert_eq!(resolve_prompt(Mode::Polish, ""), POLISH_PROMPT);
        assert_eq!(resolve_prompt(Mode::Summary, ""), SUMMARY_PROMPT);
        assert_eq!(resolve_prompt(Mode::Fulltext, ""), FULLTEXT_PROMPT);
        assert_eq!(resolve_prompt(Mode::Timestamp, ""), TIMESTAMP_PROMPT);
        assert_eq!(resolve_prompt(Mode::Custom, ""), SUMMARY_PROMPT);
        assert_eq!(resolve_prompt(Mode::Custom, "我的模板 {{title}}"), "我的模板 {{title}}");
    }
}

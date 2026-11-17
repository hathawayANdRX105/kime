//! LLM 接入：薄封装，缺省不发网络；若 `KIME_LLM_ENDPOINT` 存在且启用了 `llm`
//! feature 则可选用轻量 HTTP 请求，但不得让 CI 联网。失败/未配置时静默降级
//! 为 rules-only 建议。

// 这些类型只在 `llm` feature 下构造；默认构建（含 lib 测试目标）不编译，
// 否则会报 dead_code —— CI 的 `clippy --all-targets -D warnings` 会红。
#[cfg(feature = "llm")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "llm")]
use std::time::Duration;

/// LLM 客户端：缺省 `endpoint = None`，表示不发网络。
#[derive(Clone, Debug)]
pub struct LlmClient {
    /// OpenAI 兼容聊天端点（如 `http://127.0.0.1:8080/v1/chat/completions`）；
    /// `None` = 禁用 LLM，仅走规则建议。
    pub endpoint: Option<String>,
    pub model: String,
    pub api_key: Option<String>,
    /// 请求超时（秒），缺省 5s，避免阻塞评测主流程。
    pub timeout_secs: u64,
}

impl LlmClient {
    /// 从环境变量构造：`KIME_LLM_ENDPOINT`、`KIME_LLM_MODEL`、`KIME_LLM_API_KEY`。
    /// 未设端点时返回禁用客户端（endpoint=None）。
    pub fn from_env() -> Self {
        let endpoint = std::env::var("KIME_LLM_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty());
        let model = std::env::var("KIME_LLM_MODEL").unwrap_or_else(|_| "gpt-oss-120b".to_string());
        let api_key = std::env::var("KIME_LLM_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let timeout_secs = std::env::var("KIME_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        Self {
            endpoint,
            model,
            api_key,
            timeout_secs,
        }
    }

    /// 是否启用：端点已设且编译了 `llm` feature（否则静默降级 rules-only）。
    pub fn enabled(&self) -> bool {
        self.endpoint.is_some() && cfg!(feature = "llm")
    }

    /// 将评测摘要发给 LLM 生成建议；失败/未启用均返回 `None`（静默降级）。
    pub fn suggestions(&self, summary_json: &str) -> Option<Vec<String>> {
        if !self.enabled() {
            return None;
        }
        #[cfg(feature = "llm")]
        {
            let payload = LlmRequest {
                model: self.model.clone(),
                messages: vec![LlmMessage {
                    role: "user".to_string(),
                    content: format!(
                        "以下是中英文拼音输入法敲字评测摘要（JSON），请用中文给出 3-5 条改进建议，\
                         每条一行，不要额外解释：\n{summary_json}"
                    ),
                }],
                temperature: 0.3,
            };
            let body = serde_json::to_string(&payload).ok()?;
            let resp = post_json(&self.endpoint.as_ref()?, &body, self.timeout_secs).ok()?;
            return parse_suggestions(&resp);
        }
        #[cfg(not(feature = "llm"))]
        {
            let _ = summary_json;
            None
        }
    }
}

/// OpenAI 兼容聊天请求载荷（最小字段，避免引入 reqwest 类型泄漏）。
#[cfg(feature = "llm")]
#[derive(Serialize)]
struct LlmRequest {
    model: String,
    messages: Vec<LlmMessage>,
    temperature: f64,
}

#[cfg(feature = "llm")]
#[derive(Serialize)]
struct LlmMessage {
    role: String,
    content: String,
}

/// LLM 响应：最小解析，只取 `choices[0].message.content`。
#[cfg(feature = "llm")]
#[derive(Deserialize)]
struct LlmResponse {
    choices: Option<Vec<LlmChoice>>,
}

#[cfg(feature = "llm")]
#[derive(Deserialize)]
struct LlmChoice {
    message: LlmResponseMessage,
}

#[cfg(feature = "llm")]
#[derive(Deserialize)]
struct LlmResponseMessage {
    content: String,
}

/// 以原生 HTTP POST 发送 JSON（`llm` feature 启用时可用，复用 kime-core 已有
/// reqwest/rustls-tls，本 crate 不新增运行时系统库）。
#[cfg(feature = "llm")]
fn post_json(endpoint: &str, body: &str, timeout_secs: u64) -> anyhow::Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;
    let resp = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| anyhow::anyhow!("llm request: {e}"))?;
    let text = resp
        .text()
        .map_err(|e| anyhow::anyhow!("llm response: {e}"))?;
    Ok(text)
}

#[cfg(feature = "llm")]
fn parse_suggestions(text: &str) -> Option<Vec<String>> {
    let resp: LlmResponse = serde_json::from_str(text).ok()?;
    let mut suggestions: Vec<String> = Vec::new();
    for choice in resp.choices.unwrap_or_default() {
        let content = choice.message.content.trim();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            suggestions.push(line.to_string());
            if suggestions.len() >= 10 {
                break;
            }
        }
        if suggestions.len() >= 10 {
            break;
        }
    }
    if suggestions.is_empty() {
        None
    } else {
        Some(suggestions)
    }
}

#[cfg(all(test, feature = "llm"))]
mod tests {
    use super::*;

    #[test]
    fn from_env_disabled_without_endpoint() {
        std::env::remove_var("KIME_LLM_ENDPOINT");
        let client = LlmClient::from_env();
        assert!(!client.enabled());
    }

    #[test]
    fn parse_suggestions_empty_returns_none() {
        assert!(parse_suggestions("{}").is_none());
        assert!(parse_suggestions(r#"{"choices":[]}"#).is_none());
    }

    #[test]
    fn parse_suggestions_extracts_lines() {
        let text = r#"{"choices":[{"message":{"content":"- 建议 A\n- 建议 B\n"}]}}"#;
        let s = parse_suggestions(text).unwrap();
        assert_eq!(s, vec!["- 建议 A", "- 建议 B"]);
    }

    #[test]
    fn suggestions_disabled_returns_none() {
        let client = LlmClient {
            endpoint: None,
            model: "m".into(),
            api_key: None,
            timeout_secs: 5,
        };
        assert!(client.suggestions("{}").is_none());
    }

    #[test]
    fn cfg_llm_off_enabled_false() {
        // 编译默认不开 llm feature，enabled 必须为 false。
        assert!(!cfg!(feature = "llm"));
    }
}

//! LLM 联想增强
//!
//! 拼音切分后异步请求 LLM（OpenAI 兼容），debounce 200ms，
//! 结果经 `Engine::merge_ai` 回流主线程。

use crate::dict::Candidate;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// LLM 请求（debounce 合并）
pub struct LlmRequest {
    pub syllables: Vec<String>,
    pub timestamp: std::time::Instant,
}

/// LLM HTTP 客户端（支持同步和异步）
#[derive(Clone)]
pub struct LlmClient {
    endpoint: String,
    model: String,
    blocking_client: reqwest::blocking::Client,
    async_client: reqwest::Client,
}

impl LlmClient {
    pub fn new(endpoint: String, model: String) -> Self {
        Self {
            endpoint: endpoint.clone(),
            model: model.clone(),
            blocking_client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("Failed to build reqwest blocking client"),
            async_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("Failed to build reqwest async client"),
        }
    }

    /// 发送 LLM 请求（同步版本，供测试用）
    pub fn request_sync(&self, syllables: &[String]) -> Result<Vec<Candidate>, String> {
        let prompt = format!(
            "拼音: {}\n请给出可能的中文句子，每行一个，最多 3 个。只输出文本。",
            syllables.join("'")
        );
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 100,
        });

        let resp = self
            .blocking_client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .map_err(|e| format!("HTTP 错误: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        let json: serde_json::Value = resp.json().map_err(|e| format!("JSON 解析失败: {}", e))?;
        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");

        Ok(parse_candidates(text, syllables))
    }

    /// 发送 LLM 请求（异步版本，供壳侧后台线程用）
    pub async fn request_async(&self, syllables: Vec<String>) -> Result<Vec<Candidate>, String> {
        let prompt = format!(
            "拼音: {}\n请给出可能的中文句子，每行一个，最多 3 个。只输出文本。",
            syllables.join("'")
        );
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 100,
        });

        let resp = self
            .async_client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP 错误: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| format!("JSON 解析失败: {}", e))?;
        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");

        Ok(parse_candidates(text, &syllables))
    }
}

/// 解析 LLM 返回的文本为候选列表
fn parse_candidates(text: &str, syllables: &[String]) -> Vec<Candidate> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Candidate {
            text: l.trim().to_string(),
            pinyin: syllables.join("'"),
            freq: 1,
            ai: true,
        })
        .take(3)
        .collect()
}

/// Debounce 合并器（线程安全，供异步上下文用）
#[derive(Clone)]
pub struct Debouncer {
    last_request: Arc<Mutex<Option<std::time::Instant>>>,
    duration: Duration,
}

impl Debouncer {
    pub fn new(duration: Duration) -> Self {
        Self {
            last_request: Arc::new(Mutex::new(None)),
            duration,
        }
    }

    pub async fn should_fire(&self) -> bool {
        let mut guard = self.last_request.lock().await;
        let now = std::time::Instant::now();
        match *guard {
            Some(last) if now - last < self.duration => false,
            _ => {
                *guard = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_client_new() {
        let client = LlmClient::new(
            "http://localhost:3000/v1/chat/completions".to_string(),
            "test-model".to_string(),
        );
        assert_eq!(client.model, "test-model");
    }

    #[test]
    fn test_parse_candidates() {
        let text = "第一行\n第二行\n第三行\n第四行";
        let candidates = parse_candidates(text, &["test".to_string()]);
        assert_eq!(candidates.len(), 3); // take(3)
        assert_eq!(candidates[0].text, "第一行");
        assert!(candidates[0].ai);
        assert_eq!(candidates[0].freq, 1);
    }

    #[tokio::test]
    async fn test_debouncer_async() {
        let d = Debouncer::new(std::time::Duration::from_millis(200));
        assert!(d.should_fire().await);
        // 200ms 内不应再触发
        assert!(!d.should_fire().await);
    }
}

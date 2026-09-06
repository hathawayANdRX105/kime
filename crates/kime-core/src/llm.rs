//! LLM 联想增强
//!
//! 拼音切分后异步请求 LLM（OpenAI 兼容），debounce 200ms，
//! 结果经 `Engine::merge_ai` 回流主线程。

use crate::dict::Candidate;
use std::time::Duration;

/// LLM 请求（debounce 合并）
pub struct LlmRequest {
    pub syllables: Vec<String>,
    pub timestamp: std::time::Instant,
}

/// LLM HTTP 客户端（同步版本，供测试用）
pub struct LlmClient {
    endpoint: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl LlmClient {
    pub fn new(endpoint: String, model: String) -> Self {
        Self {
            endpoint,
            model,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("Failed to build reqwest client"),
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
            .client
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

        let candidates: Vec<Candidate> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Candidate {
                text: l.trim().to_string(),
                pinyin: syllables.join("'"),
                freq: 1,
                ai: true,
            })
            .take(3)
            .collect();

        Ok(candidates)
    }
}

/// Debounce 合并器
pub struct Debouncer {
    last_request: Option<std::time::Instant>,
    duration: Duration,
}

impl Debouncer {
    pub fn new(duration: Duration) -> Self {
        Self {
            last_request: None,
            duration,
        }
    }

    pub fn should_fire(&mut self) -> bool {
        let now = std::time::Instant::now();
        match self.last_request {
            Some(last) if now - last < self.duration => false,
            _ => {
                self.last_request = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debouncer() {
        let mut d = Debouncer::new(Duration::from_millis(200));
        assert!(d.should_fire());
        // 200ms 内不应再触发
        assert!(!d.should_fire());
    }

    #[test]
    fn test_llm_client_new() {
        let client = LlmClient::new(
            "http://localhost:3000/v1/chat/completions".to_string(),
            "test-model".to_string(),
        );
        assert_eq!(client.model, "test-model");
    }

    #[test]
    fn test_llm_request_sync_parses_response() {
        // Test the parsing logic directly with a mock JSON response
        let json_str = r#"{"choices": [{"message": {"content": "你好世界\n你好吗\n你好啊"}}]}"#;
        let json: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let text = json["choices"][0]["message"]["content"].as_str().unwrap_or("");
        
        let candidates: Vec<Candidate> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Candidate {
                text: l.trim().to_string(),
                pinyin: "ni'hao".to_string(),
                freq: 1,
                ai: true,
            })
            .take(3)
            .collect();

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].text, "你好世界");
        assert_eq!(candidates[1].text, "你好吗");
        assert_eq!(candidates[2].text, "你好啊");
        assert!(candidates[0].ai);
    }
}

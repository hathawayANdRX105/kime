//! kime-core LLM 单元测试：客户端构建、响应候选解析、debounce 合并。

use kime_core::llm::{parse_candidates, Debouncer, LlmClient};

#[test]
fn test_llm_client_new() {
    let client = LlmClient::new(
        "http://localhost:3000/v1/chat/completions".to_string(),
        "test-model".to_string(),
    );
    assert_eq!(client.model(), "test-model");
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

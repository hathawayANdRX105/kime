    fn request(&self, syllables: Vec<String>) {
        let client = self.client.clone();
        let debouncer = self.debouncer.clone();
        let sender = self.result_sender.clone();
        self.runtime.spawn(async move {
            if debouncer.should_fire().await {
                match client.request_async(syllables).await {
                    Ok(candidates) => {
                        let _ = sender.send(candidates);
                    }
                    Err(e) => log(&format!("LLM error: {}", e)),
                }
            }
        });
    }

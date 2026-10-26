//! A one-sentence summary from a local Ollama server, for a library entry.

use std::time::Duration;

use scriber_core::ports::{PortError, Summarizer, fail};
use serde_json::{Value, json};

/// How long a summary may take, from connecting to the last byte.
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(60);

/// What the model is told to do with a transcript.
pub const SYSTEM_PROMPT: &str = "You write a one-sentence summary of a dictated note, at most 15 words, plain text, no quotes, no preamble.";

/// Ollama's chat API at `url`, asking `model`.
pub struct Ollama {
    url: String,
    model: String,
    timeout: Duration,
}

impl Ollama {
    /// The server at `url` (`http://127.0.0.1:11434`), asking `model`, with
    /// [`SUMMARY_TIMEOUT`].
    #[must_use]
    pub fn new(url: &str, model: &str) -> Self {
        Self::with_timeout(url, model, SUMMARY_TIMEOUT)
    }

    /// [`Self::new`] with another timeout.
    #[must_use]
    pub fn with_timeout(url: &str, model: &str, timeout: Duration) -> Self {
        Self {
            url: url.to_string(),
            model: model.to_string(),
            timeout,
        }
    }
}

impl Summarizer for Ollama {
    fn summarize(&self, transcript: &str) -> Result<String, PortError> {
        let url = format!("{}/api/chat", self.url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "stream": false,
            "think": false,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": transcript},
            ],
        });
        let config = ureq::Agent::config_builder().timeout_global(Some(self.timeout));
        let agent = config.build().new_agent();
        let mut response = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(fail)?;
        let text = response.body_mut().read_to_string().map_err(fail)?;
        let reply: Value = serde_json::from_str(&text).map_err(fail)?;
        reply
            .pointer("/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PortError("the reply has no message content".to_string()))
    }
}

//! A local Ollama server: a one-sentence summary for a library entry, and
//! the optional cleanup pass over a finished transcript, each asking the
//! model the call names (or the adapter's default), and the listing of the
//! installed models ([`installed_models`], `GET /api/tags`, embedding models hidden).

use std::time::Duration;

use scriber_core::ports::{Cleaner, PortError, Summarizer, fail};
use serde_json::{Value, json};

/// How long a summary may take, from connecting to the last byte.
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a transcript's cleanup may take, from connecting to the last
/// byte.
pub const CLEANUP_TIMEOUT: Duration = Duration::from_secs(120);

/// How long listing the installed models may take, from connecting to the
/// last byte.
pub const TAGS_TIMEOUT: Duration = Duration::from_secs(5);

/// What the model is told to do with a transcript.
pub const SYSTEM_PROMPT: &str = "You write a one-sentence summary of a dictated note, at most 15 words, plain text, no quotes, no preamble.";

/// Ollama's chat API at `url`, asking `model` unless a call names another.
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

impl Ollama {
    /// `chosen`, or the default model when it is empty.
    fn model_for<'a>(&'a self, chosen: &'a str) -> &'a str {
        if chosen.is_empty() {
            &self.model
        } else {
            chosen
        }
    }

    /// `model`'s reply to `user` under the `system` prompt: one
    /// non-streaming chat request, thinking off, within the timeout.
    fn chat(&self, model: &str, system: &str, user: &str) -> Result<String, PortError> {
        let url = format!("{}/api/chat", self.url.trim_end_matches('/'));
        let body = json!({
            "model": model,
            "stream": false,
            "think": false,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
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
        if reply.get("done_reason").and_then(Value::as_str) == Some("length") {
            return Err(PortError("the reply was cut off".to_string()));
        }
        reply
            .pointer("/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PortError("the reply has no message content".to_string()))
    }
}

impl Summarizer for Ollama {
    fn summarize(&self, model: &str, transcript: &str) -> Result<String, PortError> {
        self.chat(self.model_for(model), SYSTEM_PROMPT, transcript)
    }
}

impl Cleaner for Ollama {
    fn clean(&self, model: &str, system: &str, request: &str) -> Result<String, PortError> {
        self.chat(self.model_for(model), system, request)
    }
}

/// Whether a `/api/tags` entry is a model that can write text.
fn is_text_model(entry: &Value) -> bool {
    let named_embed = entry
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|n| n.to_lowercase().contains("embed"));
    let completes = entry
        .get("capabilities")
        .and_then(Value::as_array)
        .is_none_or(|c| c.iter().any(|v| v.as_str() == Some("completion")));
    !named_embed && completes
}

/// The names of the models installed on the Ollama server at `url`
/// (`GET /api/tags`, the API's `ollama list`), sorted and without
/// duplicates, within [`TAGS_TIMEOUT`]. Only models that can generate text are listed: a name containing `embed` (any case) is hidden, and so is an entry whose `capabilities` array lacks `"completion"`; without such an array only the name decides.
///
/// # Errors
///
/// The server cannot be reached, answers with an error status, is too
/// slow, or its reply is not JSON with a `models` list.
pub fn installed_models(url: &str) -> Result<Vec<String>, PortError> {
    let url = format!("{}/api/tags", url.trim_end_matches('/'));
    let config = ureq::Agent::config_builder().timeout_global(Some(TAGS_TIMEOUT));
    let agent = config.build().new_agent();
    let mut response = agent.get(&url).call().map_err(fail)?;
    let text = response.body_mut().read_to_string().map_err(fail)?;
    let reply: Value = serde_json::from_str(&text).map_err(fail)?;
    let models = reply
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| PortError("the reply has no model list".to_string()))?;
    let mut names: Vec<String> = models
        .iter()
        .filter(|m| is_text_model(m))
        .filter_map(|m| m.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

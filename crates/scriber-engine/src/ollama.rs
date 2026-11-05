//! A local Ollama server: a one-sentence summary for a library entry, and
//! the optional cleanup pass over a finished transcript, each asking the
//! model the call names (or the adapter's default), and the listing of the
//! installed models ([`installed_models`], `GET /api/tags`, embedding models hidden).
//! A reply containing `</think>` is cut to the trimmed text after its last occurrence, and an empty remainder is an error.
//! The cleanup request sends Ollama's `format` with the three-field `replacements`/`learned`/`transcript` schema, and `clean` returns the structured reply, so a reply that is not JSON or has no string `transcript` is an error quoting the start of the reply.
//! It also keeps the cleanup model loaded ([`Warmer`]): `POST /api/generate` with no prompt and `keep_alive` [`KEEP_ALIVE`] loads it, and `keep_alive` 0 releases it; every chat request sends [`KEEP_ALIVE`] too, so it never shortens the model's stay.

use std::time::Duration;

use scriber_core::cleanup;
use scriber_core::ports::{Cleaner, PortError, Summarizer, fail};
use serde_json::{Value, json};

use crate::warm::Warmer;

/// The keep-alive every request to Ollama sends.
pub const KEEP_ALIVE: &str = "10m";

/// How often the cleanup model's keep-alive is renewed by default.
pub const KEEP_WARM_INTERVAL: Duration = Duration::from_secs(240);

/// How long loading a model may take, from connecting to the last byte.
pub const WARM_TIMEOUT: Duration = Duration::from_secs(60);

/// The most one release request may take, from connecting to the last byte.
pub const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// non-streaming chat request, thinking off, within the timeout; a reply containing `</think>` is cut to the trimmed text after its last occurrence (see `after_think`). With a `format` the request sends it as Ollama's structured output (the cleanup request sends the three-field `replacements`/`learned`/`transcript` schema, and `clean` returns the structured reply, so a reply that is not JSON or has no string `transcript` is an error quoting the start of the reply).
    fn chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        format: Option<&Value>,
    ) -> Result<String, PortError> {
        let url = format!("{}/api/chat", self.url.trim_end_matches('/'));
        let mut body = serde_json::Map::new();
        let base = json!({
            "model": model,
            "stream": false,
            "think": false,
            "keep_alive": KEEP_ALIVE,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        if let Value::Object(fields) = base {
            body.extend(fields);
        }
        if let Some(format) = format {
            let _previous = body.insert("format".to_string(), format.clone());
        }
        let body = Value::Object(body);
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
            .ok_or_else(|| PortError("the reply has no message content".to_string()))
            .and_then(after_think)
    }
}

impl Ollama {
    /// One `POST /api/generate` with `body` within `timeout`: any 2xx status
    /// is success, and the reply is read and ignored.
    fn generate(&self, body: &Value, timeout: Duration) -> Result<(), PortError> {
        let url = format!("{}/api/generate", self.url.trim_end_matches('/'));
        let config = ureq::Agent::config_builder().timeout_global(Some(timeout));
        let agent = config.build().new_agent();
        let mut response = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(fail)?;
        let _ignored = response.body_mut().read_to_vec();
        Ok(())
    }
}

impl Warmer for Ollama {
    fn warm(&self, model: &str) -> Result<(), PortError> {
        let body = json!({"model": self.model_for(model), "keep_alive": KEEP_ALIVE});
        self.generate(&body, self.timeout)
    }

    fn release(&self, model: &str) -> Result<(), PortError> {
        let body = json!({"model": self.model_for(model), "keep_alive": 0});
        self.generate(&body, self.timeout.min(RELEASE_TIMEOUT))
    }
}

/// The reply's text after its last `</think>` (trimmed), or the reply unchanged without one.
fn after_think(content: &str) -> Result<String, PortError> {
    match content.rsplit_once("</think>") {
        None => Ok(content.to_string()),
        Some((_, rest)) => {
            let rest = rest.trim();
            if rest.is_empty() {
                Err(PortError(
                    "the reply has nothing after </think>".to_string(),
                ))
            } else {
                Ok(rest.to_string())
            }
        }
    }
}

impl Summarizer for Ollama {
    fn summarize(&self, model: &str, transcript: &str) -> Result<String, PortError> {
        self.chat(self.model_for(model), SYSTEM_PROMPT, transcript, None)
    }
}

impl Cleaner for Ollama {
    fn clean(&self, model: &str, system: &str, request: &str) -> Result<cleanup::Reply, PortError> {
        let schema = json!({
            "type": "object",
            "properties": {
                "replacements": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "from": {"type": "string"},
                            "to": {"type": "string"},
                        },
                        "required": ["from", "to"],
                    },
                },
                "learned": {"type": "array", "items": {"type": "string"}},
                "transcript": {"type": "string"},
            },
            "required": ["replacements", "learned", "transcript"],
        });
        let content = self.chat(self.model_for(model), system, request, Some(&schema))?;
        reply_field(&content)
    }
}

/// The array at `key` in `value`, empty when it is absent or not a list of
/// `T`.
fn array_field<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Vec<T> {
    serde_json::from_value(value.get(key).cloned().unwrap_or(Value::Null)).unwrap_or_default()
}

/// The structured reply in the JSON `content`: the string `transcript` field
/// verbatim, and the two arrays (empty when absent or unusable).
fn reply_field(content: &str) -> Result<cleanup::Reply, PortError> {
    let excerpt = || {
        let start: String = content.chars().take(200).collect();
        if content.chars().count() > 200 {
            format!("{start}…")
        } else {
            start
        }
    };
    let value: Value = serde_json::from_str(content)
        .map_err(|_| PortError(format!("the reply is not JSON: {}", excerpt())))?;
    let transcript = value
        .get("transcript")
        .and_then(Value::as_str)
        .ok_or_else(|| PortError(format!("the reply has no transcript field: {}", excerpt())))?;
    Ok(cleanup::Reply {
        replacements: array_field(&value, "replacements"),
        learned: array_field(&value, "learned"),
        transcript: transcript.to_string(),
    })
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

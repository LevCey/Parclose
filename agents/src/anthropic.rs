//! The production reasoning transport: Anthropic's Messages API behind [`LLMClient`].
//!
//! This is the real model-in-the-loop. It is a drop-in replacement for the offline doubles — the
//! perceive -> reason -> act loop is identical; only the transport changes.
//!
//! Credentials and endpoint come from the environment only (never hard-coded, never committed):
//!
//! * `ANTHROPIC_API_KEY` (required) — the API key.
//! * `ANTHROPIC_MODEL` (optional) — the model id; defaults to [`DEFAULT_MODEL`].
//! * `ANTHROPIC_BASE_URL` (optional) — the Messages endpoint; defaults to [`DEFAULT_BASE_URL`].
//!
//! The request is sent with the system `curl` binary. The API key and request body are passed to
//! curl through a config on its stdin (`curl -K -`), so neither the key nor the prompt ever
//! appears in the process argument list. The HTTP call and the response parsing are kept separate
//! so the parsing is unit-testable offline ([`parse_messages_response`]); only
//! [`AnthropicClient::complete`] touches the network.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::json;

use crate::llm::{LLMClient, LLMError, Prompt};

/// Default model id, overridable via `ANTHROPIC_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-3-5-sonnet-latest";
/// Default Messages endpoint, overridable via `ANTHROPIC_BASE_URL`.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
/// The Anthropic API version header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A client for Anthropic's Messages API, transported over the system `curl` binary.
#[derive(Clone, Debug)]
pub struct AnthropicClient {
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
}

impl AnthropicClient {
    /// Builds a client from the environment. Errors if `ANTHROPIC_API_KEY` is unset or empty.
    pub fn from_env() -> Result<Self, LLMError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| LLMError::Config("ANTHROPIC_API_KEY is not set".into()))?;
        let model = std::env::var("ANTHROPIC_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Ok(Self { api_key, model, base_url, max_tokens: 1024 })
    }

    /// Overrides the default `max_tokens` for the completion.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// The model id this client targets.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Serializes the request body for a prompt (system + single user message).
    fn request_body(&self, prompt: &Prompt) -> String {
        json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
        })
        .to_string()
    }

    /// Builds the curl config (fed on stdin) carrying the URL, headers (including the key), and
    /// the JSON body — so none of them land in the process argument list.
    fn curl_config(&self, body: &str) -> String {
        format!(
            "url = \"{url}\"\n\
             request = \"POST\"\n\
             header = \"content-type: application/json\"\n\
             header = \"anthropic-version: {version}\"\n\
             header = \"x-api-key: {key}\"\n\
             data = \"{body}\"\n",
            url = self.base_url,
            version = ANTHROPIC_VERSION,
            key = curl_quote(&self.api_key),
            body = curl_quote(body),
        )
    }
}

impl LLMClient for AnthropicClient {
    fn complete(&self, prompt: &Prompt) -> Result<String, LLMError> {
        let config = self.curl_config(&self.request_body(prompt));

        // `--fail-with-body` exits non-zero on an HTTP error but still emits the body, so an API
        // error object reaches `parse_messages_response` instead of being swallowed.
        let mut child = Command::new("curl")
            .args(["-sS", "--fail-with-body", "-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| LLMError::Transport(format!("could not run curl: {e}")))?;

        child
            .stdin
            .take()
            .ok_or_else(|| LLMError::Transport("curl stdin unavailable".into()))?
            .write_all(config.as_bytes())
            .map_err(|e| LLMError::Transport(format!("writing curl config failed: {e}")))?;

        let output = child
            .wait_with_output()
            .map_err(|e| LLMError::Transport(format!("curl did not complete: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LLMError::Transport(format!(
                "curl returned no body (exit {:?}): {}",
                output.status.code(),
                stderr.trim()
            )));
        }
        parse_messages_response(&stdout)
    }
}

/// Escapes a value for a curl config double-quoted string (`"\\"` and `"\""`).
fn curl_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Extracts the assistant's text from a Messages API response body, concatenating all text
/// content blocks. Surfaces an API `error` object as a provider error.
pub fn parse_messages_response(body: &str) -> Result<String, LLMError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| LLMError::Provider(format!("invalid JSON: {e}")))?;

    if let Some(err) = value.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(|m| m.to_string())
            .unwrap_or_else(|| err.to_string());
        return Err(LLMError::Provider(message));
    }

    let blocks = value
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LLMError::Provider("response had no content array".into()))?;

    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
        }
    }

    if text.trim().is_empty() {
        Err(LLMError::EmptyCompletion)
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_text_completion() {
        let body = r#"{"id":"msg_1","type":"message","role":"assistant",
            "content":[{"type":"text","text":"{\"side\":\"subscribe\"}"}],
            "model":"claude-3-5-sonnet-latest","stop_reason":"end_turn"}"#;
        assert_eq!(parse_messages_response(body).unwrap(), r#"{"side":"subscribe"}"#);
    }

    #[test]
    fn concatenates_multiple_text_blocks() {
        let body = r#"{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#;
        assert_eq!(parse_messages_response(body).unwrap(), "ab");
    }

    #[test]
    fn surfaces_api_errors() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let err = parse_messages_response(body).unwrap_err();
        assert_eq!(err, LLMError::Provider("invalid x-api-key".into()));
    }

    #[test]
    fn empty_content_is_an_empty_completion() {
        let body = r#"{"content":[]}"#;
        assert_eq!(parse_messages_response(body).unwrap_err(), LLMError::EmptyCompletion);
    }

    #[test]
    fn curl_quote_escapes_quotes_and_backslashes() {
        assert_eq!(curl_quote(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}

//! The Ollama summarizer and cleanup model against a plain HTTP stub on
//! 127.0.0.1.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use scriber_engine::{
        CLEANUP_TIMEOUT, Cleaner as _, KEEP_ALIVE, KEEP_WARM_INTERVAL, Ollama, RELEASE_TIMEOUT,
        SUMMARY_TIMEOUT, SYSTEM_PROMPT, Summarizer as _, TAGS_TIMEOUT, WARM_TIMEOUT, Warmer as _,
        installed_models,
    };
    use serde_json::{Value, json};

    /// A request as the stub read it: its request line and its body.
    type Request = (String, String);

    /// Answers one request with `response` and hands the request over;
    /// answers with the base URL.
    fn serve(response: &str) -> (String, Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let response = response.to_string();
        let (tx, rx) = mpsc::channel();
        let _server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            let _first = reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                let _header = reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            reader.into_inner().write_all(response.as_bytes()).unwrap();
            tx.send((
                request_line.trim_end().to_string(),
                String::from_utf8(body).unwrap(),
            ))
            .unwrap();
        });
        (base, rx)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const REPLY: &str = r#"{"model":"m","message":{"role":"assistant","content":" A note about the board. "},"done":true}"#;

    #[test]
    fn the_timeout_and_prompt_are_pinned() {
        assert_eq!(SUMMARY_TIMEOUT, Duration::from_secs(60), "timeout");
        assert_eq!(CLEANUP_TIMEOUT, Duration::from_secs(120), "cleanup timeout");
        assert_eq!(
            SYSTEM_PROMPT,
            "You write a one-sentence summary of a dictated note, at most 15 words, plain text, no quotes, no preamble.",
            "prompt"
        );
    }

    fn reply(content: &str) -> String {
        ok(&json!({"message": {"role": "assistant", "content": content}, "done": true, "done_reason": "stop"}).to_string())
    }

    #[test]
    fn a_leaked_closing_think_tag_is_cut_for_cleanup() {
        let (base, _rx) = serve(&reply(
            "Fix the parser.\n</think>\n\n{\"transcript\":\"Fix the parser.\"}",
        ));
        let out = Ollama::with_timeout(&base, "m", CLEANUP_TIMEOUT)
            .clean("", "S", "R")
            .unwrap();
        assert_eq!(out, "Fix the parser.");
    }

    #[test]
    fn a_leading_think_block_is_dropped() {
        let (base, _rx) = serve(&reply(
            "<think>\nweighing it\n</think>\n\n A note about the board. ",
        ));
        let out = Ollama::new(&base, "m").summarize("", "x").unwrap();
        assert_eq!(out, "A note about the board.");
    }

    #[test]
    fn the_last_closing_tag_wins() {
        let (base, _rx) = serve(&reply("first</think>second</think> third "));
        assert_eq!(Ollama::new(&base, "m").summarize("", "x").unwrap(), "third");
    }

    #[test]
    fn nothing_after_the_tag_is_an_error() {
        let want = "the reply has nothing after </think>";
        let (base1, _rx1) = serve(&reply("Fix the parser.\n</think>\n \n"));
        let e1 = Ollama::new(&base1, "m").summarize("", "x").unwrap_err();
        assert_eq!(e1.0, want);
        let (base2, _rx2) = serve(&reply("Fix the parser.\n</think>\n \n"));
        let e2 = Ollama::with_timeout(&base2, "m", CLEANUP_TIMEOUT)
            .clean("", "S", "R")
            .unwrap_err();
        assert_eq!(e2.0, want);
        let (base3, _rx3) = serve(&reply("</think>"));
        let e3 = Ollama::new(&base3, "m").summarize("", "x").unwrap_err();
        assert_eq!(e3.0, want);
    }

    #[test]
    fn a_half_open_tag_is_unchanged() {
        let (base, _rx) = serve(&reply(" <think> half open "));
        let out = Ollama::new(&base, "m").summarize("", "x").unwrap();
        assert_eq!(out, " <think> half open ");
    }

    #[test]
    fn a_reply_is_its_content_as_is() {
        let (base, rx) = serve(&ok(REPLY));
        let summary = Ollama::new(&base, "qwen3.8:27b-mlx")
            .summarize("", "Check the board.")
            .unwrap();
        assert_eq!(summary, " A note about the board. ", "raw content");
        let (line, body) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(line, "POST /api/chat HTTP/1.1", "the request line");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "qwen3.8:27b-mlx",
                "stream": false,
                "think": false,
                "keep_alive": "10m",
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": "Check the board."},
                ],
            }),
            "the body"
        );
        assert!(body.get("format").is_none(), "no format on a summary");
    }

    #[test]
    fn a_trailing_slash_posts_to_the_same_path() {
        let (base, rx) = serve(&ok(REPLY));
        let _summary = Ollama::new(&format!("{base}/"), "m")
            .summarize("", "x")
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap().0,
            "POST /api/chat HTTP/1.1",
            "one slash"
        );
    }

    #[test]
    fn empty_content_is_returned_for_the_core_to_refuse() {
        let (base, _rx) = serve(&ok(r#"{"message":{"role":"assistant","content":""}}"#));
        assert_eq!(
            Ollama::new(&base, "m").summarize("", "x").unwrap(),
            "",
            "empty"
        );
    }

    #[test]
    fn failures_are_errors() {
        for response in [
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
            ok("not json"),
            ok(r#"{"message":{"role":"assistant"}}"#),
            ok(r#"{"message":{"content":3}}"#),
        ] {
            let (base, _rx) = serve(&response);
            assert!(
                Ollama::new(&base, "m").summarize("", "x").is_err(),
                "{response}"
            );
        }
    }

    #[test]
    fn a_refused_connection_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(
            Ollama::new(&base, "m").summarize("", "x").is_err(),
            "refused"
        );
    }

    #[test]
    fn a_server_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let _server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(10));
            drop(stream);
        });
        let started = Instant::now();
        let ollama = Ollama::with_timeout(&base, "m", Duration::from_millis(300));
        assert!(ollama.summarize("", "x").is_err(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(5), "promptly");
    }

    #[test]
    fn a_cleanup_sends_its_system_prompt_and_request() {
        let (base, rx) = serve(&reply(r#"{"transcript":" A note about the board. "}"#));
        let reply = Ollama::with_timeout(&base, "m", CLEANUP_TIMEOUT)
            .clean("", "S", "R")
            .unwrap();
        assert_eq!(reply, " A note about the board. ", "the field verbatim");
        let (line, body) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(line, "POST /api/chat HTTP/1.1", "the request line");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "m",
                "stream": false,
                "think": false,
                "keep_alive": "10m",
                "format": {
                    "type": "object",
                    "properties": {"transcript": {"type": "string"}},
                    "required": ["transcript"],
                },
                "messages": [
                    {"role": "system", "content": "S"},
                    {"role": "user", "content": "R"},
                ],
            }),
            "the body"
        );
    }

    #[test]
    fn a_reply_cut_off_at_its_length_is_an_error() {
        let cut = ok(
            r#"{"message":{"role":"assistant","content":"A note"},"done":true,"done_reason":"length"}"#,
        );
        let (summary_base, _summary_rx) = serve(&cut);
        let summary = Ollama::new(&summary_base, "m").summarize("", "x");
        assert!(
            summary.as_ref().is_err_and(|e| e.0.contains("cut off")),
            "summary: {summary:?}"
        );
        let (clean_base, _clean_rx) = serve(&cut);
        let cleaned = Ollama::new(&clean_base, "m").clean("", "S", "R");
        assert!(
            cleaned.as_ref().is_err_and(|e| e.0.contains("cut off")),
            "cleanup: {cleaned:?}"
        );
        let (stop_base, _stop_rx) = serve(&ok(
            r#"{"message":{"role":"assistant","content":"{\"transcript\":\"A note\"}"},"done":true,"done_reason":"stop"}"#,
        ));
        assert_eq!(
            Ollama::new(&stop_base, "m").clean("", "S", "R").unwrap(),
            "A note",
            "a finished reply"
        );
    }

    /// The `"model"` the request made by `call` against a stub named.
    fn model_sent(response: &str, call: impl FnOnce(&str)) -> Value {
        let (base, rx) = serve(response);
        call(&base);
        let body: Value =
            serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap().1).unwrap();
        body.get("model").cloned().unwrap()
    }

    #[test]
    fn each_call_asks_its_model_or_the_default() {
        let summary = |chosen: &'static str| {
            model_sent(&ok(REPLY), move |base| {
                let _reply = Ollama::new(base, "d").summarize(chosen, "t").unwrap();
            })
        };
        assert_eq!(summary(""), json!("d"), "summary: the default");
        assert_eq!(summary("m2"), json!("m2"), "summary: chosen");
        let cleanup = |chosen: &'static str| {
            model_sent(&reply(r#"{"transcript":"x"}"#), move |base| {
                let _reply = Ollama::with_timeout(base, "d", CLEANUP_TIMEOUT)
                    .clean(chosen, "S", "R")
                    .unwrap();
            })
        };
        assert_eq!(cleanup(""), json!("d"), "cleanup: the default");
        assert_eq!(cleanup("c2"), json!("c2"), "cleanup: chosen");
    }

    fn cleaned(content: &str) -> Result<String, String> {
        let (base, _rx) = serve(&reply(content));
        Ollama::with_timeout(&base, "m", CLEANUP_TIMEOUT)
            .clean("", "S", "R")
            .map_err(|e| e.0)
    }

    #[test]
    fn a_cleanup_returns_its_transcript_field() {
        assert_eq!(
            cleaned(r#"{"transcript":" Fix the parser. "}"#),
            Ok(" Fix the parser. ".to_string()),
            "verbatim"
        );
        assert_eq!(cleaned(r#"{"transcript":""}"#), Ok(String::new()), "empty");
        assert_eq!(
            cleaned(r#"{"transcript":"x","note":"y"}"#),
            Ok("x".to_string()),
            "extra keys"
        );
    }

    #[test]
    fn a_cleanup_reply_that_is_not_json_is_an_error() {
        let text = "Fix the parser. I also removed a repeat.";
        assert_eq!(
            cleaned(text),
            Err(format!("the reply is not JSON: {text}")),
            "prose"
        );
        let long = "a".repeat(250);
        assert_eq!(
            cleaned(&long),
            Err(format!("the reply is not JSON: {}…", "a".repeat(200))),
            "250 chars"
        );
        let exact = "a".repeat(200);
        assert_eq!(
            cleaned(&exact),
            Err(format!("the reply is not JSON: {exact}")),
            "200 chars"
        );
        let wide = "é".repeat(201);
        assert_eq!(
            cleaned(&wide),
            Err(format!("the reply is not JSON: {}…", "é".repeat(200))),
            "multibyte"
        );
    }

    #[test]
    fn a_cleanup_reply_without_a_string_transcript_is_an_error() {
        for content in [
            r#"["x"]"#,
            "{}",
            r#"{"text":"x"}"#,
            r#"{"transcript":3}"#,
            r#"{"transcript":null}"#,
        ] {
            assert_eq!(
                cleaned(content),
                Err(format!("the reply has no transcript field: {content}")),
                "{content}"
            );
        }
    }

    #[test]
    fn the_tags_timeout_is_pinned() {
        assert_eq!(TAGS_TIMEOUT, Duration::from_secs(5), "tags timeout");
    }

    /// What `installed_models` answers for a stub replying `response`, and
    /// the request line it sent; `slash` adds a trailing `/` to the URL.
    fn listed(response: &str, slash: bool) -> (Result<Vec<String>, String>, String) {
        let (base, rx) = serve(response);
        let url = if slash { format!("{base}/") } else { base };
        let models = installed_models(&url).map_err(|e| e.0);
        (
            models,
            rx.recv_timeout(Duration::from_secs(5))
                .map(|r| r.0)
                .unwrap_or_default(),
        )
    }

    #[test]
    fn the_installed_models_are_listed_sorted_once_each() {
        let (models, line) = listed(
            &ok(
                r#"{"models":[{"name":"qwen3:8b"},{"name":"gemma3:4b"},{"name":"qwen3:8b"},{"model":"x"},{"name":7}]}"#,
            ),
            false,
        );
        assert_eq!(models.unwrap(), ["gemma3:4b", "qwen3:8b"], "names");
        assert_eq!(line, "GET /api/tags HTTP/1.1", "the request line");
        let (none, _) = listed(&ok(r#"{"models":[]}"#), false);
        assert_eq!(none, Ok(vec![]), "none");
    }

    #[test]
    fn embedding_models_are_not_listed() {
        let (models, _) = listed(
            &ok(
                r#"{"models":[{"name":"qwen3.8:27b-mlx","capabilities":["completion","vision","tools","thinking"]},{"name":"qwen3-embedding:0.6b","capabilities":["tools","thinking","embedding"]},{"name":"nomic:latest","capabilities":["embedding"]},{"name":"empty:1","capabilities":[]},{"name":"odd:1","capabilities":[7,"Completion"]},{"name":"mixed:1","capabilities":[7,"completion"]},{"name":"bge-Embed-chat:1","capabilities":["completion"]}]}"#,
            ),
            false,
        );
        assert_eq!(models.unwrap(), ["mixed:1", "qwen3.8:27b-mlx"], "names");
    }

    #[test]
    fn without_capabilities_only_the_name_hides_a_model() {
        let (models, _) = listed(
            &ok(
                r#"{"models":[{"name":"gemma3:4b"},{"name":"mxbai-EMBED-large:latest"},{"name":"llama3:8b","capabilities":null},{"name":"all-minilm-embedding:1","capabilities":"completion"},{"name":"phi4:14b","capabilities":{"completion":true}}]}"#,
            ),
            false,
        );
        assert_eq!(
            models.unwrap(),
            ["gemma3:4b", "llama3:8b", "phi4:14b"],
            "names"
        );
    }

    #[test]
    fn a_trailing_slash_lists_from_the_same_path() {
        let (_, line) = listed(&ok(r#"{"models":[]}"#), true);
        assert_eq!(line, "GET /api/tags HTTP/1.1", "one slash");
    }

    #[test]
    fn a_listing_that_fails_is_an_error() {
        let (no_list, _) = listed(&ok("{}"), false);
        assert!(
            no_list.as_ref().is_err_and(|e| e.contains("no model list")),
            "no list: {no_list:?}"
        );
        for response in [
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
            ok("not json"),
            ok(r#"{"models":3}"#),
        ] {
            assert!(listed(&response, false).0.is_err(), "{response}");
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(installed_models(&base).is_err(), "refused");
    }

    #[test]
    fn the_keep_warm_constants_are_pinned() {
        assert_eq!(KEEP_ALIVE, "10m", "keep alive");
        assert_eq!(KEEP_WARM_INTERVAL, Duration::from_secs(240), "interval");
        assert_eq!(WARM_TIMEOUT, Duration::from_secs(60), "warm timeout");
        assert_eq!(RELEASE_TIMEOUT, Duration::from_secs(2), "release timeout");
        assert!(
            KEEP_WARM_INTERVAL < Duration::from_secs(600),
            "renewed within the keep alive"
        );
    }

    const FAILED: &str =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// The request line and parsed body `call` sent to a stub answering `{}`.
    fn generated(slash: bool, call: impl FnOnce(&Ollama) -> bool) -> (String, Value) {
        let (base, rx) = serve(&ok("{}"));
        let url = if slash { format!("{base}/") } else { base };
        assert!(call(&Ollama::with_timeout(&url, "m", WARM_TIMEOUT)), "ok");
        let (line, body) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        (line, serde_json::from_str(&body).unwrap())
    }

    #[test]
    fn a_warm_loads_the_model_with_the_keep_alive() {
        let (line, body) = generated(false, |o| o.warm("llama3:8b").is_ok());
        assert_eq!(line, "POST /api/generate HTTP/1.1", "the request line");
        assert_eq!(
            body,
            json!({"model": "llama3:8b", "keep_alive": "10m"}),
            "the body"
        );
    }

    #[test]
    fn a_release_unloads_the_model_now() {
        let (line, body) = generated(false, |o| o.release("llama3:8b").is_ok());
        assert_eq!(line, "POST /api/generate HTTP/1.1", "the request line");
        assert_eq!(
            body,
            json!({"model": "llama3:8b", "keep_alive": 0}),
            "the body"
        );
    }

    #[test]
    fn an_unnamed_model_is_the_default_for_warm_and_release() {
        let (_, warm) = generated(false, |o| o.warm("").is_ok());
        assert_eq!(warm, json!({"model": "m", "keep_alive": "10m"}), "warm");
        let (_, release) = generated(false, |o| o.release("").is_ok());
        assert_eq!(release, json!({"model": "m", "keep_alive": 0}), "release");
    }

    #[test]
    fn a_trailing_slash_warms_and_releases_at_the_same_path() {
        let (warm, _) = generated(true, |o| o.warm("m").is_ok());
        assert_eq!(warm, "POST /api/generate HTTP/1.1", "warm");
        let (release, _) = generated(true, |o| o.release("m").is_ok());
        assert_eq!(release, "POST /api/generate HTTP/1.1", "release");
    }

    #[test]
    fn a_failed_status_is_an_error_for_warm_and_release() {
        let (warm_base, _warm_rx) = serve(FAILED);
        assert!(
            Ollama::with_timeout(&warm_base, "m", WARM_TIMEOUT)
                .warm("m")
                .is_err(),
            "warm"
        );
        let (release_base, _release_rx) = serve(FAILED);
        assert!(
            Ollama::with_timeout(&release_base, "m", WARM_TIMEOUT)
                .release("m")
                .is_err(),
            "release"
        );
    }

    #[test]
    fn a_refused_connection_is_an_error_for_warm_and_release() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let ollama = Ollama::with_timeout(&base, "m", WARM_TIMEOUT);
        assert!(ollama.warm("m").is_err(), "warm");
        assert!(ollama.release("m").is_err(), "release");
    }

    /// A server that accepts one connection and never answers it, for 10 s.
    fn silent_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let _server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(10));
            drop(stream);
        });
        base
    }

    #[test]
    fn a_warm_waits_for_its_own_timeout() {
        let base = silent_url();
        let started = Instant::now();
        let ollama = Ollama::with_timeout(&base, "m", Duration::from_millis(200));
        assert!(ollama.warm("m").is_err(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(1), "promptly");
    }

    #[test]
    fn a_release_waits_at_most_the_release_timeout() {
        let base = silent_url();
        let started = Instant::now();
        let ollama = Ollama::with_timeout(&base, "m", WARM_TIMEOUT);
        assert!(ollama.release("m").is_err(), "timed out");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(1900),
            "the cap: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(5), "not the warm timeout");
    }
}

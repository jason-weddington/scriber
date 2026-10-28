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
        CLEANUP_TIMEOUT, Cleaner as _, Ollama, SUMMARY_TIMEOUT, SYSTEM_PROMPT, Summarizer as _,
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

    #[test]
    fn a_reply_is_its_content_as_is() {
        let (base, rx) = serve(&ok(REPLY));
        let summary = Ollama::new(&base, "qwen3.8:27b-mlx")
            .summarize("Check the board.")
            .unwrap();
        assert_eq!(summary, " A note about the board. ", "raw content");
        let (line, body) = rx.recv().unwrap();
        assert_eq!(line, "POST /api/chat HTTP/1.1", "the request line");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "qwen3.8:27b-mlx",
                "stream": false,
                "think": false,
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": "Check the board."},
                ],
            }),
            "the body"
        );
    }

    #[test]
    fn a_trailing_slash_posts_to_the_same_path() {
        let (base, rx) = serve(&ok(REPLY));
        let _summary = Ollama::new(&format!("{base}/"), "m")
            .summarize("x")
            .unwrap();
        assert_eq!(rx.recv().unwrap().0, "POST /api/chat HTTP/1.1", "one slash");
    }

    #[test]
    fn empty_content_is_returned_for_the_core_to_refuse() {
        let (base, _rx) = serve(&ok(r#"{"message":{"role":"assistant","content":""}}"#));
        assert_eq!(Ollama::new(&base, "m").summarize("x").unwrap(), "", "empty");
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
                Ollama::new(&base, "m").summarize("x").is_err(),
                "{response}"
            );
        }
    }

    #[test]
    fn a_refused_connection_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(Ollama::new(&base, "m").summarize("x").is_err(), "refused");
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
        assert!(ollama.summarize("x").is_err(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(5), "promptly");
    }

    #[test]
    fn a_cleanup_sends_its_system_prompt_and_request() {
        let (base, rx) = serve(&ok(REPLY));
        let reply = Ollama::with_timeout(&base, "m", CLEANUP_TIMEOUT)
            .clean("S", "R")
            .unwrap();
        assert_eq!(reply, " A note about the board. ", "raw content");
        let (line, body) = rx.recv().unwrap();
        assert_eq!(line, "POST /api/chat HTTP/1.1", "the request line");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "m",
                "stream": false,
                "think": false,
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
        let summary = Ollama::new(&summary_base, "m").summarize("x");
        assert!(
            summary.as_ref().is_err_and(|e| e.0.contains("cut off")),
            "summary: {summary:?}"
        );
        let (clean_base, _clean_rx) = serve(&cut);
        let cleaned = Ollama::new(&clean_base, "m").clean("S", "R");
        assert!(
            cleaned.as_ref().is_err_and(|e| e.0.contains("cut off")),
            "cleanup: {cleaned:?}"
        );
        let (stop_base, _stop_rx) = serve(&ok(
            r#"{"message":{"role":"assistant","content":"A note"},"done":true,"done_reason":"stop"}"#,
        ));
        assert_eq!(
            Ollama::new(&stop_base, "m").clean("S", "R").unwrap(),
            "A note",
            "a finished reply"
        );
    }
}

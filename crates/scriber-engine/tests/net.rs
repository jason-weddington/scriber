//! The HTTP downloader against a plain HTTP server on 127.0.0.1.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use scriber_engine::Downloader as _;
    use scriber_engine::HttpDownloader;

    /// Serves `responses` in turn, one per connection, and answers with the
    /// base URL.
    fn serve(responses: &[&str]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let responses: Vec<String> = responses.iter().map(|r| (*r).to_string()).collect();
        let _server = thread::spawn(move || {
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                // The request, up to the blank line that ends its headers.
                while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    line.clear();
                }
                reader.into_inner().write_all(response.as_bytes()).unwrap();
            }
        });
        base
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd";

    fn body(url: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let _n = HttpDownloader::default()
            .open(url)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn a_body_is_read_back() {
        let base = serve(&[OK]);
        assert_eq!(body(&format!("{base}/m")), b"abcd", "the body");
    }

    #[test]
    fn a_redirect_is_followed() {
        let base = serve(&[
            "HTTP/1.1 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            OK,
        ]);
        assert_eq!(body(&format!("{base}/m")), b"abcd", "the redirected body");
    }

    #[test]
    fn an_error_status_is_an_error() {
        let base =
            serve(&["HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"]);
        assert!(
            HttpDownloader::default()
                .open(&format!("{base}/m"))
                .is_err(),
            "404"
        );
    }
}

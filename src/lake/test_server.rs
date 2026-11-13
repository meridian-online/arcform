//! A static file server for tests: serves a directory over plain http on
//! `127.0.0.1`, answering GET and HEAD, with single byte ranges, so DuckDB's
//! httpfs can attach a catalogue and read its data files the way a reader does
//! over https.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

pub struct TestServer {
    pub port: u16,
}

impl TestServer {
    /// Serve `root` until the test process ends.
    pub fn serve(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a test port");
        let port = listener.local_addr().expect("the test port").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let root = root.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, &root);
                });
            }
        });
        TestServer { port }
    }

    /// `http://127.0.0.1:<port>/<path>`.
    pub fn url(&self, path: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}",
            self.port,
            path.trim_start_matches('/')
        )
    }
}

/// The byte range a `Range: bytes=…` header asks for, clamped to `len`.
fn range(header: &str, len: usize) -> Option<(usize, usize)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let (start, end) = if start.is_empty() {
        let suffix: usize = end.parse().ok()?;
        (len.saturating_sub(suffix), len.checked_sub(1)?)
    } else {
        let start: usize = start.parse().ok()?;
        let end = if end.is_empty() {
            len.checked_sub(1)?
        } else {
            end.parse::<usize>().ok()?.min(len.checked_sub(1)?)
        };
        (start, end)
    };
    (start <= end && end < len).then_some((start, end))
}

fn handle(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        let mut range_header = None;
        let mut close = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "range" => range_header = Some(value.trim().to_string()),
                    "connection" => close = value.trim().eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }

        let path = target.split('?').next().unwrap_or_default();
        let relative = Path::new(path.trim_start_matches('/'));
        let safe = relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        let body = if safe {
            std::fs::read(root.join(relative)).ok()
        } else {
            None
        };
        let head = method.eq_ignore_ascii_case("HEAD");
        match body {
            Some(bytes) if method == "GET" || head => {
                let etag: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let (status, slice, content_range) =
                    match range_header.as_deref().and_then(|r| range(r, bytes.len())) {
                        Some((s, e)) => (
                            "206 Partial Content",
                            &bytes[s..=e],
                            Some(format!("bytes {s}-{e}/{}", bytes.len())),
                        ),
                        None => ("200 OK", &bytes[..], None),
                    };
                let mut headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\
                     Content-Type: application/octet-stream\r\nETag: \"{etag}\"\r\n\
                     Last-Modified: Thu, 01 Jan 2026 00:00:00 GMT\r\n",
                    slice.len()
                );
                if let Some(cr) = content_range {
                    headers.push_str(&format!("Content-Range: {cr}\r\n"));
                }
                headers.push_str("\r\n");
                out.write_all(headers.as_bytes())?;
                if !head {
                    out.write_all(slice)?;
                }
            }
            Some(_) => {
                out.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")?;
            }
            None => {
                out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?;
            }
        }
        out.flush()?;
        if close {
            return Ok(());
        }
    }
}

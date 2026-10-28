//! What a start check needs to poll a freshly started release build, shared by `bin/bundle.rs`
//! and `bin/package.rs` (installers/03-the-bundle-agent-bundle.md section 3.10): a free port for
//! the scratch agent, and a plain-socket GET loop waiting for its health check to come up.
use std::io::{Read, Write};

/// A port nothing is listening on right now, for the start check's own agent (connector/tests.rs's pattern).
pub fn free_port() -> std::io::Result<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()
        .map(|a| a.port())
}

/// A bare GET over a plain socket: the start check has no need for `reqwest`'s TLS or redirects.
pub fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connecting: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("setting a read timeout: {e}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("writing the request: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("reading the response: {e}"))?;
    Ok(response.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

/// Polls `GET /api/health` on `port` until it answers `version`, up to 60 seconds.
pub fn wait_for_health(port: u16, version: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut last_err = "no attempt reached it".to_string();
    while std::time::Instant::now() < deadline {
        match http_get(port, "/api/health") {
            Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(answer) if answer.get("version").and_then(|v| v.as_str()) == Some(version) => return Ok(()),
                Ok(answer) => last_err = format!("GET /api/health answered {answer}, not version {version:?}"),
                Err(e) => last_err = format!("GET /api/health answered JSON that didn't parse ({e}): {body}"),
            },
            Err(e) => last_err = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(format!(
        "127.0.0.1:{port} didn't answer its health within 60 seconds: {last_err}"
    ))
}

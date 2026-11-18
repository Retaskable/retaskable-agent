//! The real sweep against a real Chromium: a tiny loopback site with known accessibility problems on `/bad` and
//! none on `/good`. The test is `#[ignore]`d (it needs node, the two npm modules and a Chromium).
//!
//! Run it in the official Playwright image (Chromium and its libraries are in it; the test binary is built on
//! the host and mounted, and the modules are installed once into a mounted folder):
//!
//! ```text
//! cargo test --no-run --lib   # prints the test binary's path, TEST_BIN below
//! docker run --rm --ipc=host --user pwuser -v "$TEST_BIN":/t/agent-test:ro -v "$TOOLS":/opt/a11y-tools \
//!   -e RETASKABLE_A11Y_TOOLS=/opt/a11y-tools -e RETASKABLE_A11Y_NO_SANDBOX=1 \
//!   mcr.microsoft.com/playwright:v1.63.0-noble /t/agent-test --ignored --nocapture smoke_
//! ```
//!
//! Without the `-v $TOOLS` and `-e RETASKABLE_A11Y_TOOLS` pair the launcher installs the two modules on demand
//! (needs network; add `-e HOME=/tmp`); that fallback passes the same way.
//!
//! `$TOOLS` holds `node_modules` with `playwright-core@1.63.0` and `axe-core@4.10.3`
//! (`npm install --ignore-scripts` there); the built Docker image has the same at `/opt/a11y-tools`.
use std::io::{Read, Write};
use std::net::TcpListener;

use super::*;
use crate::jobs::a11y_sweep::{BrowserSweeper, SweepResult, SweepRun, Sweeper};

const BAD: &str = r##"<!doctype html><html><body>
<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" width="40" height="40">
<p style="color:#ccc;background:#fff">Faint grey text on white</p>
<button id="trap" onkeydown="if(event.key==='Tab'||event.key==='Escape'){event.preventDefault();this.focus()}">Trap</button>
</body></html>"##;
const GOOD: &str = r##"<!doctype html><html lang="en"><head><title>Good page</title></head><body><main>
<h1>Good page</h1><p style="color:#222;background:#fff">Dark text on white</p>
<a href="/bad" class="l">A link</a><style>.l{outline:none}.l:focus{outline:2px solid #00f}</style></main></body></html>"##;

/// Serves `/bad` and `/good` on an ephemeral loopback port until the process ends.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let body = match head.split_whitespace().nth(1) {
                Some("/bad") => BAD,
                Some("/good") => GOOD,
                _ => "",
            };
            let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

fn failed(r: &SweepResult, id: &str, route: usize) -> bool {
    let all = r.rules.iter().chain(&r.checks);
    all.filter(|v| v.id == id).any(|v| v.pass.get(route) == Some(&false))
}

#[tokio::test]
#[ignore]
async fn smoke_a_real_chromium_finds_the_known_problems() {
    let base = serve();
    let tools = tempfile::tempdir().unwrap();
    let routes = ["/bad".to_string(), "/good".to_string()];
    let run = BrowserSweeper(PlaywrightLauncher::new(tools.path().join("a11y-tools")))
        .sweep(&base, &routes, None)
        .await;
    let SweepRun::Ok { result, .. } = run else {
        panic!("{run:?}")
    };
    eprintln!("failures: {:#?}", result.failures);
    assert_eq!(result.routes, 2);
    assert!(
        !failed(&result, "scan-focus-visible", 1),
        "a lone link with a focus outline is visible"
    );
    for id in [
        "image-alt",
        "color-contrast",
        "document-title",
        "html-has-lang",
        "scan-keyboard-trap",
        "scan-page-title",
    ] {
        assert!(failed(&result, id, 0), "{id} should fail on /bad");
        assert!(!failed(&result, id, 1), "{id} should not fail on /good");
    }
}

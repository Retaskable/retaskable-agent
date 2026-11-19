//! The HTTP client every long-lived store caller shares the settings of, and the pool that rebuilds it.
//!
//! A pooled HTTP/2 connection can die without the agent being told (a DNS or network blip, a NAT dropping the
//! flow): every later call then queues on it and fails as `error sending request`, while a fresh process works.
//! So each client pings its HTTP/2 connections (and drops one that doesn't answer), retires idle ones, and
//! bounds connecting; and as a last resort [`Pool`] builds a new client (an empty connection pool) after
//! [`REBUILD_AFTER`] failures in a row that never got an answer from the store.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;

/// Consecutive calls that failed at the connection level before the client is rebuilt.
pub const REBUILD_AFTER: u32 = 3;

/// A client for a long-lived caller: `timeout` bounds one whole request; the rest keeps a dead connection from
/// sticking (HTTP/2 pings even while idle, a short idle pool, TCP keepalive, a bounded connect).
pub fn builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
}

/// [`builder`], built.
pub fn build(timeout: Duration) -> reqwest::Client {
    builder(timeout).build().unwrap_or_default()
}

/// The whole chain of a failure, "error sending request for url (..): client error (Connect): dns error: ...",
/// so a log names the real cause (DNS, connect, TLS, GOAWAY, timeout), not just the outermost words.
pub fn chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut next = err.source();
    while let Some(e) = next {
        let s = e.to_string();
        if !out.contains(&s) {
            out.push_str(": ");
            out.push_str(&s);
        }
        next = e.source();
    }
    out
}

/// Whether a failed call never got an answer from the store (connect, timeout, a connection that died), as
/// opposed to a bad request or an unreadable answer.
pub fn is_connection_failure(e: &reqwest::Error) -> bool {
    !(e.is_status() || e.is_builder() || e.is_redirect() || e.is_decode())
}

/// A client that is replaced after [`REBUILD_AFTER`] connection-level failures in a row. Any answer from the
/// store (whatever its status) resets the count.
pub struct Pool {
    client: Mutex<reqwest::Client>,
    make: Arc<dyn Fn() -> reqwest::Client + Send + Sync>,
    failures: AtomicU32,
}

impl Pool {
    /// A pool of `timeout`-bounded clients.
    pub fn new(timeout: Duration) -> Self {
        Self::with(build(timeout), move || build(timeout))
    }

    /// A pool that starts with `client` and builds new ones with `make`.
    pub fn with(client: reqwest::Client, make: impl Fn() -> reqwest::Client + Send + Sync + 'static) -> Self {
        Self {
            client: Mutex::new(client),
            make: Arc::new(make),
            failures: AtomicU32::new(0),
        }
    }

    /// The current client (a cheap handle on its connection pool).
    pub fn client(&self) -> reqwest::Client {
        crate::sync::lock(&self.client).clone()
    }

    /// Records how a call ended; true when this one rebuilt the client.
    pub fn record(&self, connection_failure: bool) -> bool {
        if !connection_failure {
            self.failures.store(0, Ordering::SeqCst);
            return false;
        }
        if self.failures.fetch_add(1, Ordering::SeqCst) + 1 < REBUILD_AFTER {
            return false;
        }
        self.failures.store(0, Ordering::SeqCst);
        *crate::sync::lock(&self.client) = (self.make)();
        tracing::warn!(
            "http: {REBUILD_AFTER} calls in a row never got an answer; rebuilt the client (fresh connections)"
        );
        true
    }

    /// Records the outcome of a send, passing it through.
    pub fn note<T>(&self, res: Result<T, reqwest::Error>) -> Result<T, reqwest::Error> {
        self.record(res.as_ref().err().is_some_and(is_connection_failure));
        res
    }
}

/// An existing client becomes a pool that rebuilds with the shared settings (callers and tests that pass one).
impl From<reqwest::Client> for Pool {
    fn from(client: reqwest::Client) -> Self {
        Self::with(client, || build(Duration::from_secs(30)))
    }
}

#[cfg(test)]
mod tests;

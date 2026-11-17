use super::*;
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn rebuilds_after_n_connection_failures_in_a_row_and_an_answer_resets_the_count() {
    let made = Arc::new(AtomicUsize::new(0));
    let m = made.clone();
    let pool = Pool::with(reqwest::Client::new(), move || {
        m.fetch_add(1, Ordering::SeqCst);
        reqwest::Client::new()
    });
    for _ in 0..REBUILD_AFTER - 1 {
        assert!(!pool.record(true));
    }
    // Any answer (even an error status) means the connection works: start counting again.
    assert!(!pool.record(false));
    for _ in 0..REBUILD_AFTER - 1 {
        assert!(!pool.record(true));
    }
    assert_eq!(made.load(Ordering::SeqCst), 0);
    assert!(pool.record(true));
    assert_eq!(made.load(Ordering::SeqCst), 1);
    // The count starts over after a rebuild.
    assert!(!pool.record(true));
}

#[tokio::test]
async fn chain_names_every_cause_once() {
    let err = reqwest::Client::new()
        .get("http://127.0.0.1:1/ops")
        .send()
        .await
        .unwrap_err();
    assert!(is_connection_failure(&err));
    let text = chain(&err);
    assert!(text.starts_with("error sending request"), "{text}");
    assert!(text.len() > err.to_string().len(), "the causes are named: {text}");
}

/// Accepts connections and reads them forever without answering, until told to answer instead.
async fn flaky_server(answer: Arc<std::sync::atomic::AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/ops", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let answer = answer.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    if answer.load(Ordering::SeqCst) {
                        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
                    }
                }
            });
        }
    });
    url
}

/// A store that swallows requests and then comes back: the pool rebuilds its client after the blackholed calls
/// and the next call gets through, with no restart. (A connection that stays dead for good, as a half-dead
/// HTTP/2 one does, isn't reproducible on loopback; this covers the rebuild path end to end.)
#[tokio::test]
async fn recovers_once_the_store_answers_again() {
    let answer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let url = flaky_server(answer.clone()).await;
    let timeout = Duration::from_millis(150);
    let pool = Pool::new(timeout);
    for _ in 0..REBUILD_AFTER {
        let res = pool.note(pool.client().post(&url).send().await);
        assert!(res.unwrap_err().is_timeout());
    }
    answer.store(true, Ordering::SeqCst);
    let res = pool.note(pool.client().post(&url).send().await).unwrap();
    assert_eq!(res.text().await.unwrap(), "ok");
}

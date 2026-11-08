use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};

use super::*;
use crate::link::pending::BacklogWaiting;
use crate::restart::RestartState;
use crate::tray::agent::{LinkRegistry, Opener, Quit, SettingsLink};
use crate::view::AgentView;

struct Links(tokio::sync::watch::Sender<u64>);
impl LinkRegistry for Links {
    fn waiting(&self, _: &str) -> Option<(String, String)> {
        None
    }
    fn flowing(&self, _: &str) -> bool {
        false
    }
    fn start(&self, _: &str) -> bool {
        true
    }
    fn backlog(&self) -> Option<BacklogWaiting> {
        None
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.subscribe()
    }
}

struct Nothing;
impl Opener for Nothing {
    fn open_url(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn open_file(&self, _: &Path) -> Result<()> {
        Ok(())
    }
    fn notify(&self, _: &str) {}
}
impl SettingsLink for Nothing {
    fn settings_link(&self) -> String {
        "http://127.0.0.1:4700/settings".into()
    }
}

#[derive(Default)]
struct Quits(AtomicUsize);
impl Quit for Quits {
    fn now(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn exit(&self, _: i32) {}
}

struct Fake {
    to_agent: DuplexStream,
    from_agent: BufReader<DuplexStream>,
}

impl Fake {
    async fn line(&mut self) -> Option<serde_json::Value> {
        let mut s = String::new();
        (self.from_agent.read_line(&mut s).await.unwrap() > 0).then(|| serde_json::from_str(&s).unwrap())
    }
}

struct Up {
    handle: TrayHandle,
    fake: Fake,
    quits: Arc<Quits>,
}

fn icons() -> Icons {
    std::array::from_fn(|i| encode_icon(&[i as u8]))
}

/// A tray over a fake helper that has said it is ready.
async fn up(on_stopped: impl FnOnce(String) + Send + 'static) -> Up {
    let (view, pending) = HelperView::new(icons());
    let quits = Arc::new(Quits::default());
    let deps = TrayDeps {
        view: Arc::new(AgentView::blank()),
        clouds: vec![],
        links: Arc::new(Links(tokio::sync::watch::channel(0).0)),
        sessions: Arc::new(|| 0),
        restart: Arc::new(RestartState::default()),
        opener: Arc::new(Nothing),
        quit: quits.clone(),
        settings: Arc::new(Nothing),
        login: None,
        log_file: None,
        windows: false,
        link_backlog: Arc::new(|| {}),
    };
    let agent = TrayAgent::new(deps, view.clone());
    let first = agent.frame();
    let first = helper::Frame {
        icon: view.icons[slot(first.icon)].clone(),
        tooltip: first.tooltip,
        menu: first.menu,
    };
    let (agent_r, mut to_agent) = duplex(1 << 16);
    let (from_agent, agent_w) = duplex(1 << 16);
    to_agent.write_all(b"{\"type\":\"ready\"}\n").await.unwrap();
    let helper = TrayHelper::connect(agent_r, agent_w, &first, Duration::from_secs(5))
        .await
        .unwrap();
    let handle = run(helper, pending, agent, view, on_stopped);
    Up {
        handle,
        fake: Fake {
            to_agent,
            from_agent: BufReader::new(from_agent),
        },
        quits,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_helper_that_stops_leaves_the_agent_running_headless() {
    let (tx, rx) = oneshot::channel();
    let mut tray = up(move |why| {
        let _ = tx.send(why);
    })
    .await;
    assert!(tray.fake.line().await.is_some(), "the whole menu first");
    tray.fake.to_agent.shutdown().await.unwrap(); // the helper's output ends: it has stopped
    let why = tokio::time::timeout(Duration::from_secs(5), rx).await.unwrap().unwrap();
    assert!(why.contains("stopped"), "{why}");
    // Its input was closed (the lines end), and the agent didn't quit.
    while tray.fake.line().await.is_some() {}
    assert_eq!(tray.quits.0.load(Ordering::SeqCst), 0);
    tokio::task::spawn_blocking(move || tray.handle.shutdown())
        .await
        .unwrap(); // harmless after the helper went
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_tells_the_helper_to_exit_and_closes_its_input() {
    let mut tray = up(|_| panic!("the helper didn't stop by itself")).await;
    assert!(tray.fake.line().await.is_some());
    let handle = tray.handle;
    tokio::task::spawn_blocking(move || handle.shutdown()).await.unwrap();
    let mut lines = vec![];
    while let Some(line) = tray.fake.line().await {
        lines.push(line);
    }
    assert_eq!(lines.last().unwrap()["type"], "exit");
}

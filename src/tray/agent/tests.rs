use std::sync::atomic::AtomicUsize;

use super::*;
use crate::view::PauseState;

#[derive(Default)]
pub(crate) struct FakeView {
    frames: Mutex<Vec<Frame>>,
    closed: AtomicBool,
    /// A real view to pass everything on to.
    pub(crate) forward: std::sync::OnceLock<Arc<dyn TrayView>>,
}
impl TrayView for FakeView {
    fn draw(&self, frame: &Frame) -> Result<()> {
        self.frames.lock().unwrap().push(frame.clone());
        self.forward.get().map_or(Ok(()), |v| v.draw(frame))
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(v) = self.forward.get() {
            v.close();
        }
    }
}

struct FakeConn {
    state: Mutex<CloudLink>,
    forgot: AtomicUsize,
}
impl FakeConn {
    fn new(state: CloudLink) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
            forgot: AtomicUsize::new(0),
        })
    }
}
impl CloudConn for FakeConn {
    fn connected(&self) -> CloudLink {
        *self.state.lock().unwrap()
    }
    fn forget(&self) -> Result<()> {
        self.forgot.fetch_add(1, Ordering::SeqCst);
        *self.state.lock().unwrap() = CloudLink::NotLinked;
        Ok(())
    }
}

pub(crate) struct FakeLinks {
    started: Mutex<Vec<String>>,
    pub(crate) backlog: Mutex<Option<BacklogWaiting>>,
    tx: tokio::sync::watch::Sender<u64>,
}
impl LinkRegistry for FakeLinks {
    fn waiting(&self, _: &str) -> Option<(String, String)> {
        None
    }
    fn flowing(&self, url: &str) -> bool {
        self.started.lock().unwrap().iter().any(|u| u == url)
    }
    fn start(&self, url: &str) -> bool {
        self.started.lock().unwrap().push(url.to_string());
        true
    }
    fn backlog(&self) -> Option<BacklogWaiting> {
        self.backlog.lock().unwrap().clone()
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.tx.subscribe()
    }
}

#[derive(Default)]
struct FakeOpener {
    urls: Mutex<Vec<String>>,
    files: Mutex<Vec<PathBuf>>,
    notes: Mutex<Vec<String>>,
}
impl Opener for FakeOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        self.urls.lock().unwrap().push(url.into());
        Ok(())
    }
    fn open_file(&self, file: &Path) -> Result<()> {
        self.files.lock().unwrap().push(file.into());
        Ok(())
    }
    fn notify(&self, text: &str) {
        self.notes.lock().unwrap().push(text.into());
    }
}

#[derive(Default)]
pub(crate) struct FakeQuit {
    pub(crate) now: AtomicUsize,
    codes: Mutex<Vec<i32>>,
}
impl Quit for FakeQuit {
    fn now(&self) {
        self.now.fetch_add(1, Ordering::SeqCst);
    }
    fn exit(&self, code: i32) {
        self.codes.lock().unwrap().push(code);
    }
}

struct FakeLogin(AtomicBool);
impl Login for FakeLogin {
    fn is_on(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn set(&self, on: bool) -> Result<()> {
        self.0.store(on, Ordering::SeqCst);
        Ok(())
    }
}

struct FakeSettings(AtomicUsize);
impl SettingsLink for FakeSettings {
    fn settings_link(&self) -> String {
        format!(
            "http://127.0.0.1:4700/settings#t{}",
            self.0.fetch_add(1, Ordering::SeqCst)
        )
    }
}

pub(crate) struct Rig {
    pub(crate) agent: Arc<TrayAgent>,
    pub(crate) view: Arc<FakeView>,
    prod: Arc<FakeConn>,
    local: Arc<FakeConn>,
    pub(crate) links: Arc<FakeLinks>,
    opener: Arc<FakeOpener>,
    pub(crate) quit: Arc<FakeQuit>,
    login: Arc<FakeLogin>,
    sessions: Arc<AtomicUsize>,
    pub(crate) agent_view: Arc<AgentView>,
    restart: Arc<RestartState>,
    backlog_links: Arc<AtomicUsize>,
}

pub(crate) fn rig(release: bool) -> Rig {
    let projects = ["a", "b"]
        .iter()
        .map(|id| crate::test_support::project_view(id))
        .collect();
    let mut v = AgentView::blank();
    v.projects = projects;
    v.paused = PauseState::default();
    let agent_view = Arc::new(v);
    let (prod, local) = (FakeConn::new(CloudLink::Connected), FakeConn::new(CloudLink::Connected));
    let links = Arc::new(FakeLinks {
        started: Mutex::default(),
        backlog: Mutex::default(),
        tx: tokio::sync::watch::channel(0).0,
    });
    let (opener, quit, view) = (
        Arc::new(FakeOpener::default()),
        Arc::new(FakeQuit::default()),
        Arc::new(FakeView::default()),
    );
    let login = Arc::new(FakeLogin(AtomicBool::new(false)));
    let sessions = Arc::new(AtomicUsize::new(0));
    let restart = Arc::new(RestartState::default());
    let backlog_links = Arc::new(AtomicUsize::new(0));
    let (s, b) = (sessions.clone(), backlog_links.clone());
    let deps = TrayDeps {
        view: agent_view.clone(),
        clouds: vec![
            CloudSlot {
                name: "Production".into(),
                url: "https://retaskable.com".into(),
                conn: prod.clone(),
            },
            CloudSlot {
                name: "Local".into(),
                url: "http://localhost:8787".into(),
                conn: local.clone(),
            },
        ],
        links: links.clone(),
        sessions: Arc::new(move || s.load(Ordering::SeqCst)),
        restart: restart.clone(),
        opener: opener.clone(),
        quit: quit.clone(),
        settings: Arc::new(FakeSettings(AtomicUsize::new(0))),
        login: release.then(|| login.clone() as Arc<dyn Login>),
        log_file: release.then(|| PathBuf::from("agent.log")),
        windows: false,
        link_backlog: Arc::new(move || {
            b.fetch_add(1, Ordering::SeqCst);
        }),
    };
    let agent = TrayAgent::new(deps, view.clone());
    agent.refresh();
    Rig {
        agent,
        view,
        prod,
        local,
        links,
        opener,
        quit,
        login,
        sessions,
        agent_view,
        restart,
        backlog_links,
    }
}

fn last(r: &Rig) -> Frame {
    r.view.frames.lock().unwrap().last().unwrap().clone()
}

fn draws(r: &Rig) -> usize {
    r.view.frames.lock().unwrap().len()
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(60)).await;
}

#[tokio::test]
async fn redraws_on_the_tick_and_at_once_on_a_pause_change() {
    let r = rig(false);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(r.agent.clone().run_every(Duration::from_millis(500), rx));
    settle().await;
    let before = draws(&r);
    r.agent_view.paused.set("a", Some("x".into()));
    r.agent_view.paused.set("b", Some("x".into()));
    settle().await; // far under the tick: only the pause change can have drawn
    assert!(draws(&r) > before);
    assert!(last(&r).tooltip.contains("Paused"));
    let before = draws(&r);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(draws(&r) > before, "the tick draws again");
    tx.send(()).unwrap();
    task.await.unwrap();
    assert!(r.view.closed.load(Ordering::SeqCst));
}

#[test]
fn link_starts_the_flow_and_forget_forgets_only_that_cloud() {
    let r = rig(false);
    *r.local.state.lock().unwrap() = CloudLink::NotLinked;
    r.agent.refresh();
    r.agent.click("link.1.do");
    assert_eq!(*r.links.started.lock().unwrap(), ["http://localhost:8787"]);
    r.agent.click("link.0.do"); // Forget Production
    assert_eq!(r.prod.forgot.load(Ordering::SeqCst), 1);
    assert_eq!(r.local.forgot.load(Ordering::SeqCst), 0);
}

#[test]
fn open_on_opens_the_clouds_page_and_refuses_anything_not_http() {
    let r = rig(false);
    *r.local.state.lock().unwrap() = CloudLink::NotLinked;
    r.agent.refresh();
    r.agent.click("open");
    assert_eq!(*r.opener.urls.lock().unwrap(), ["https://retaskable.com/"]);
    r.agent.act(&Action::OpenUrl("file:///etc/passwd".into())).unwrap_err();
    r.agent.act(&Action::OpenUrl("javascript:alert(1)".into())).unwrap_err();
    assert_eq!(r.opener.urls.lock().unwrap().len(), 1);
}

#[test]
fn pause_pauses_every_project_and_resume_undoes_it() {
    let r = rig(false);
    r.agent.click("pause");
    assert!(r.agent_view.paused.reason("a").is_some() && r.agent_view.paused.reason("b").is_some());
    assert!(last(&r).menu.flatten().iter().any(|i| i.text == "Resume this computer"));
    r.agent.click("pause");
    assert!(r.agent_view.paused.reason("a").is_none() && r.agent_view.paused.reason("b").is_none());
}

#[test]
fn settings_opens_a_fresh_token_link_each_time() {
    let r = rig(false);
    r.agent.click("settings");
    r.agent.click("settings");
    let urls = r.opener.urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 2);
    assert_ne!(urls[0], urls[1]);
    assert!(urls[0].starts_with("http://127.0.0.1:4700/settings#"));
}

#[test]
fn start_at_login_toggles_and_open_the_log_opens_the_file() {
    let r = rig(true);
    r.agent.click("start_at_login");
    assert!(r.login.is_on());
    assert!(
        last(&r)
            .menu
            .flatten()
            .iter()
            .any(|i| i.id == "start_at_login" && i.checked)
    );
    r.agent.click("start_at_login");
    assert!(!r.login.is_on());
    r.agent.click("open_log");
    assert_eq!(*r.opener.files.lock().unwrap(), [PathBuf::from("agent.log")]);
}

#[test]
fn quit_now_closes_the_view_and_quits() {
    let r = rig(false);
    r.agent.click("quit");
    assert_eq!(r.quit.now.load(Ordering::SeqCst), 1);
    assert!(r.view.closed.load(Ordering::SeqCst));
}

#[test]
fn quit_when_idle_waits_for_running_work_then_exits_zero() {
    let r = rig(false);
    r.sessions.store(2, Ordering::SeqCst);
    r.agent.refresh();
    r.agent.click("quit_when_idle");
    assert!(last(&r).menu.items[0].text.contains("quitting when idle (2 sessions)"));
    assert!(r.quit.codes.lock().unwrap().is_empty());
    let guard = r.restart.hold("a merge");
    r.sessions.store(0, Ordering::SeqCst);
    r.agent.refresh();
    assert!(r.quit.codes.lock().unwrap().is_empty(), "held work still runs");
    drop(guard);
    r.agent.refresh();
    r.agent.refresh();
    assert_eq!(*r.quit.codes.lock().unwrap(), [0]);
    assert_eq!(r.quit.now.load(Ordering::SeqCst), 0);
}

#[test]
fn a_click_on_a_hidden_or_greyed_item_does_nothing() {
    let r = rig(false); // not a release build: these are hidden
    r.agent.click("start_at_login");
    r.agent.click("open_log");
    r.agent.click("open_app");
    r.agent.click("quit_when_idle"); // hidden while nothing runs
    r.agent.click("link.2.do"); // no such cloud
    r.agent.click("status"); // greyed out
    assert!(r.opener.files.lock().unwrap().is_empty() && r.opener.urls.lock().unwrap().is_empty());
    assert!(r.links.started.lock().unwrap().is_empty());
    assert!(!r.quit.codes.lock().unwrap().contains(&0));
}

#[test]
fn the_notification_fires_once_per_wait() {
    let r = rig(false);
    assert!(r.opener.notes.lock().unwrap().is_empty());
    *r.links.backlog.lock().unwrap() = Some(BacklogWaiting::approve("https://store.example.com/link?code=A"));
    r.agent.refresh();
    r.agent.refresh();
    assert_eq!(r.opener.notes.lock().unwrap().len(), 1);
    assert!(r.opener.notes.lock().unwrap()[0].contains("store.example.com"));
    *r.links.backlog.lock().unwrap() = None;
    r.agent.refresh();
    *r.links.backlog.lock().unwrap() = Some(BacklogWaiting::unlinked(
        "store.example.com",
        "https://store.example.com/link",
    ));
    r.agent.refresh();
    assert_eq!(r.opener.notes.lock().unwrap().len(), 2);
    r.agent.click("request.backlog.do"); // Link again
    assert_eq!(r.backlog_links.load(Ordering::SeqCst), 1);
}

#[test]
fn a_project_paused_by_the_store_counts_as_paused() {
    let r = rig(false);
    for p in ["a", "b"] {
        r.agent_view
            .item_commands
            .set_flag(p, crate::view::OBEY_SHARED_PAUSE_FLAG, true);
        r.agent_view
            .store_paused
            .set(p, crate::view::StorePause::Paused("Paused for everyone".into()));
    }
    r.agent.refresh();
    assert!(last(&r).menu.flatten().iter().any(|i| i.text == "Resume this computer"));
}

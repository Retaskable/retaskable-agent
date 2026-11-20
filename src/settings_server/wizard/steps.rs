//! The wizard's steps as pure logic (docs/specs/setup-wizard.md section 3): the facts about this computer go in, each
//! step's state and the words for it come out. Nothing here touches a file, a process or the network, so every step's
//! check is tested by building the facts it reads. Nothing here ever holds a token.
use serde_json::{Value, json};

/// Where a step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Done,
    /// Needs the person; `todo` says what.
    Needs,
    /// Something tried to work and didn't; `failed` says why.
    Failed,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Done => "done",
            State::Needs => "needs",
            State::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: &'static str,
    pub title: &'static str,
    pub state: State,
    pub detail: String,
    /// A done step's answer in a few words, for the Done checklist ("jamez667", "signed in").
    pub summary: String,
    pub todo: Option<String>,
    pub failed: Option<String>,
    pub data: Value,
}

impl Step {
    fn new(id: &'static str, title: &'static str) -> Self {
        Self {
            id,
            title,
            state: State::Needs,
            detail: String::new(),
            summary: String::new(),
            todo: None,
            failed: None,
            data: Value::Null,
        }
    }

    fn done(mut self, detail: impl Into<String>) -> Self {
        self.state = State::Done;
        self.detail = detail.into();
        self
    }

    fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    fn needs(mut self, todo: impl Into<String>) -> Self {
        self.state = State::Needs;
        self.todo = Some(todo.into());
        self
    }

    fn failed(mut self, why: impl Into<String>) -> Self {
        self.state = State::Failed;
        self.failed = Some(why.into());
        self
    }

    fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }

    pub fn json(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "state": self.state.name(), "detail": self.detail, "summary": self.summary,
            "todo": self.todo, "failed": self.failed, "data": self.data,
        })
    }
}

/// What a person approves: the store's page with the code filled in, and the code.
#[derive(Clone, Debug, PartialEq)]
pub struct Approve {
    pub label: String,
    pub url: Option<String>,
    pub code: String,
}

/// Step 1's facts.
#[derive(Clone, Debug, Default)]
pub struct LinkFacts {
    pub cloud_name: String,
    /// retaskable.com's address for this computer's cloud: where the person approves commands.
    pub cloud_url: String,
    pub cloud: bool,
    pub store: bool,
    /// The CI runner's own link; `None` when CI is off or not installed (nothing to link).
    pub runner: Option<bool>,
    pub waiting: Option<Approve>,
    pub error: Option<String>,
    pub working: bool,
}

/// Where a saved or given token came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Unset,
    Saved,
    Env,
    Unreadable,
}

/// A token GitHub accepted.
#[derive(Clone, Debug, PartialEq)]
pub struct GhOk {
    pub login: String,
    pub name: Option<String>,
    pub identity: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GithubFacts {
    pub token: Token,
    /// `None`: nothing to check (no token and `gh` isn't signed in either).
    pub check: Option<Result<GhOk, String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClaudeStatus {
    SignedIn(String),
    SignedOut,
    Missing,
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct ClaudeFacts {
    /// Which secret holds it: `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`.
    pub saved: Option<&'static str>,
    pub status: ClaudeStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRow {
    pub id: String,
    pub repo: String,
    /// Why this computer's work on it is paused (`paused.json`), `None` when it is working.
    pub paused: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectFacts {
    pub on_computer: Vec<ProjectRow>,
    /// `repo` and the step the one add is at.
    pub adding: Option<(String, String)>,
    pub add_failed: Option<(String, String)>,
}

/// A project's commands that wait for the owner (`CommandGates::waiting`).
#[derive(Clone, Debug, PartialEq)]
pub struct WaitingCommands {
    /// The project's id on this computer, which approving names.
    pub id: String,
    pub project: String,
    /// The waiting version (`CommandGates::approve` takes it, so only that version can be approved).
    pub version: u64,
    pub text: String,
    pub lines: Vec<crate::allowed_commands::CommandLine>,
    pub by: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CiFacts {
    /// Slots saved as 0: the person turned it off.
    pub off: bool,
    /// `running`, `stopped`, `unlinked` or `notInstalled`, as the programs panel says.
    pub state: String,
    pub linked: bool,
    /// Where its GitHub token comes from, in words; `None` when it has none.
    pub token: Option<String>,
    /// `ok`, `missing` or `stopped`, from its status file.
    pub docker: Option<String>,
    pub last_error: Option<String>,
    pub last_check: Option<String>,
    pub repos: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub computer: String,
    pub can_act: bool,
    /// Why this computer can't save anything (the data folder isn't writable, no key to seal with).
    pub storage_problem: Option<String>,
    pub links: LinkFacts,
    pub github: GithubFacts,
    pub claude: ClaudeFacts,
    pub projects: ProjectFacts,
    pub waiting: Vec<WaitingCommands>,
    pub ci: CiFacts,
}

/// The steps in order, each with its state and words.
pub fn steps(f: &Facts) -> Vec<Step> {
    let mut all = vec![
        link_step(f),
        github_step(f),
        claude_step(f),
        projects_step(f),
        commands_step(f),
        ci_step(f),
    ];
    let done_so_far = all.iter().all(|s| s.state == State::Done);
    let projects = &f.projects.on_computer;
    let all_paused = !projects.is_empty() && projects.iter().all(|p| p.paused.is_some());
    all.push(match (done_so_far, all_paused) {
        (true, false) => Step::new("done", "Done").done("Everything this computer needs is set up."),
        // Set up, but a computer whose every project is paused works on nothing: say so plainly.
        (true, true) => Step::new("done", "Done")
            .done("This computer is set up, but every project is paused, so it is not working on anything yet. Press Start working on it beside a project in step 4.")
            .data(json!({ "allPaused": true })),
        (false, _) => Step::new("done", "Done").needs("Finish the steps above."),
    });
    all
}

/// The first step that isn't done: where the wizard is, `None` when every step is.
pub fn current(steps: &[Step]) -> Option<&'static str> {
    steps.iter().find(|s| s.state != State::Done).map(|s| s.id)
}

/// Whether every required step is done: the page opens as a wizard until then. CI counts: it is on by default, and
/// the person turns it off in its own step.
pub fn complete(steps: &[Step]) -> bool {
    steps.iter().all(|s| s.state == State::Done)
}

fn link_step(f: &Facts) -> Step {
    let l = &f.links;
    let step = Step::new("link", "Link to retaskable.com");
    let runner_done = l.runner.unwrap_or(true);
    let data = json!({
        "cloud": { "name": l.cloud_name, "linked": l.cloud, "url": l.cloud_url },
        "store": { "linked": l.store },
        "runner": l.runner,
        "working": l.working,
        "approve": l.waiting.as_ref().map(|a| json!({ "label": a.label, "url": a.url, "code": a.code })),
    });
    if l.cloud && l.store && runner_done {
        return step
            .done(format!("Linked to {} and the shared backlog.", l.cloud_name))
            .summary(format!(
                "{}, shared backlog{}",
                l.cloud_name,
                if l.runner.is_some() { ", CI runner" } else { "" }
            ))
            .data(data);
    }
    if let Some(why) = &l.error {
        return step.failed(why.clone()).data(data);
    }
    let left = usize::from(!l.cloud) + usize::from(!l.store) + usize::from(!runner_done);
    let todo = match (&l.waiting, f.can_act) {
        (Some(a), _) => format!(
            "Open the approval page, check the code {} and approve it ({} approval{} left).",
            a.code,
            left,
            if left == 1 { "" } else { "s" }
        ),
        (None, false) => "Open this page on the computer itself to link it.".to_string(),
        (None, true) => format!(
            "Press Link this computer, then approve it on retaskable.com ({left} approval{}: {}).",
            if left == 1 { "" } else { "s" },
            [
                (!l.cloud).then_some("this computer"),
                (!l.store).then_some("the shared backlog"),
                (!runner_done).then_some("the CI runner"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
        ),
    };
    step.needs(todo).data(data)
}

fn github_step(f: &Facts) -> Step {
    let step = Step::new("github", "GitHub token");
    if let Some(why) = &f.storage_problem {
        return step.failed(why.clone()).data(json!({ "storage": true }));
    }
    let data = json!({ "token": token_word(f.github.token) });
    match (&f.github.check, f.github.token) {
        (_, Token::Unreadable) => step
            .failed("The saved token can't be opened with this computer's key. Paste it again.")
            .data(data),
        (Some(Ok(ok)), _) => {
            let who = ok.name.as_ref().map_or(ok.login.clone(), |n| format!("{n} ({})", ok.login));
            let mut detail = format!("Signed in to GitHub as {who}.");
            if let Some(id) = &ok.identity {
                detail.push_str(&format!(" Commits are made as {id}."));
            }
            step.done(detail)
                .summary(ok.login.clone())
                .data(json!({ "token": token_word(f.github.token), "login": ok.login, "name": ok.name }))
        }
        (Some(Err(why)), _) => step.failed(why.clone()).data(data),
        (None, _) => step
            .needs(
                "Create a token with Create a token (it opens GitHub with the scopes filled in), copy it and paste it here.",
            )
            .data(data),
    }
}

fn token_word(t: Token) -> &'static str {
    match t {
        Token::Unset => "unset",
        Token::Saved => "saved",
        Token::Env => "env",
        Token::Unreadable => "unreadable",
    }
}

fn claude_step(f: &Facts) -> Step {
    let step = Step::new("claude", "Claude");
    let c = &f.claude;
    let data = json!({ "saved": c.saved });
    match &c.status {
        ClaudeStatus::SignedIn(detail) => step.done(detail.clone()).summary("signed in").data(data),
        ClaudeStatus::Missing => step
            .failed("Claude Code isn't installed in this program's image: update the agent (docker compose pull).")
            .data(data),
        ClaudeStatus::Unknown(why) => step.failed(why.clone()).data(data),
        ClaudeStatus::SignedOut if c.saved.is_some() => step
            .failed("Claude Code didn't accept the saved token or key. Paste a new one.")
            .data(data),
        ClaudeStatus::SignedOut => step
            .needs(
                "In a terminal on any computer with Claude Code, run `claude setup-token`, sign in, and paste the token it prints; or paste an Anthropic API key instead.",
            )
            .data(data),
    }
}

fn projects_step(f: &Facts) -> Step {
    let step = Step::new("projects", "Projects");
    let p = &f.projects;
    let data = json!({
        "onComputer": p.on_computer.iter().map(|r| json!({ "id": r.id, "repo": r.repo, "paused": r.paused }))
            .collect::<Vec<_>>(),
        "adding": p.adding.as_ref().map(|(repo, step)| json!({ "repo": repo, "step": step })),
    });
    if let Some((repo, at)) = &p.adding {
        return step.needs(format!("Linking {repo}: {at}.")).data(data);
    }
    if let Some((repo, why)) = &p.add_failed {
        return step.failed(format!("Couldn't link {repo}: {why}")).data(data);
    }
    if p.on_computer.is_empty() {
        return step
            .needs("Tick a project below and press Link. It is cloned onto this computer and starts paused.")
            .data(data);
    }
    let names: Vec<&str> = p.on_computer.iter().map(|r| r.repo.as_str()).collect();
    let paused = p.on_computer.iter().filter(|r| r.paused.is_some()).count();
    let note = match paused {
        0 => String::new(),
        n if n == p.on_computer.len() => {
            " Every one is paused: press Start working on it beside a project to begin.".to_string()
        }
        n => format!(" {n} paused: press Start working on it beside a project to begin."),
    };
    step.done(format!("On this computer: {}.{note}", names.join(", ")))
        .summary(projects_summary(p))
        .data(data)
}

/// "5: a, b, c, … (working on: x, y)": how many, the first names, and which are not paused.
fn projects_summary(p: &ProjectFacts) -> String {
    let name = |r: &ProjectRow| r.repo.rsplit('/').next().unwrap_or(&r.repo).to_string();
    let all: Vec<String> = p.on_computer.iter().map(name).collect();
    let working: Vec<String> = p.on_computer.iter().filter(|r| r.paused.is_none()).map(name).collect();
    let shown = all.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = if all.len() > 3 { ", …" } else { "" };
    let state = if working.is_empty() {
        "all paused".to_string()
    } else {
        format!("working on: {}", working.join(", "))
    };
    format!("{} \u{2014} {shown}{more} ({state})", all.len())
}

fn commands_step(f: &Facts) -> Step {
    let step = Step::new("commands", "Approve commands");
    let rows: Vec<Value> = f
        .waiting
        .iter()
        .map(|w| json!({ "id": w.id, "project": w.project, "version": w.version, "text": w.text, "lines": w.lines, "by": w.by }))
        .collect();
    if f.waiting.is_empty() {
        return step
            .done("No project has commands waiting for you.")
            .summary("nothing waiting")
            .data(json!({ "waiting": rows }));
    }
    let projects: Vec<&str> = f.waiting.iter().map(|w| w.project.as_str()).collect();
    step.needs(format!(
        "Approve the commands waiting for {}: this computer runs them only once you have.",
        projects.join(", ")
    ))
    .data(json!({ "waiting": rows }))
}

fn ci_step(f: &Facts) -> Step {
    let step = Step::new("ci", "CI");
    let c = &f.ci;
    let data = json!({
        "off": c.off, "state": c.state, "linked": c.linked, "token": c.token, "docker": c.docker,
        "lastCheck": c.last_check, "repos": c.repos,
    });
    if c.off {
        return step
            .done("Off: you turned CI off on this computer.")
            .summary("off")
            .data(data);
    }
    if c.state == "notInstalled" {
        return step
            .needs("The CI runner isn't running on this computer. Start it (docker compose up -d) or turn CI off here.")
            .data(data);
    }
    if c.state == "stopped" {
        return step
            .failed("The CI runner stopped. See its log: docker compose logs ci.")
            .data(data);
    }
    if !c.linked {
        return step
            .needs("Link the runner in step 1: it has its own link to the shared backlog.")
            .data(data);
    }
    let Some(token) = &c.token else {
        return step
            .needs("The runner has no GitHub token. Save one in the GitHub step: the runner uses it too.")
            .data(data);
    };
    match c.docker.as_deref() {
        Some("ok") => {}
        Some("stopped") => {
            return step
                .failed("Docker isn't answering for the runner. Start Docker Desktop.")
                .data(data);
        }
        Some(_) => {
            return step
                .failed(
                    "The runner can't reach Docker: check docker compose logs ci (it says which socket it looked for).",
                )
                .data(data);
        }
        None => {
            return step
                .needs("Waiting for the runner to report. This takes a few seconds.")
                .data(data);
        }
    }
    if let Some(why) = &c.last_error {
        return step.failed(why.clone()).data(data);
    }
    let last = c
        .last_check
        .clone()
        .unwrap_or_else(|| "No check has run yet: the next pull request starts one.".to_string());
    let summary = match &c.last_check {
        Some(l) => format!(
            "running, {}",
            l.trim_start_matches("Last check: ").trim_end_matches('.')
        ),
        None => "running, no checks yet".to_string(),
    };
    step.done(format!("Running and linked; token: {token}. {last}"))
        .summary(summary)
        .data(data)
}

/// The scopes a classic token needs, and what `gh api -i user` told about the token.
pub const REQUIRED_SCOPES: [&str; 2] = ["repo", "workflow"];

/// `gh api -i user`'s output: the account and the token's scopes. A classic token has `X-OAuth-Scopes`; a fine-grained
/// one has none to read, so only a classic token's missing scope is a failure here.
pub fn parse_gh_user(output: &str) -> Result<(Value, Option<Vec<String>>), String> {
    let (head, body) = output
        .split_once("\r\n\r\n")
        .or_else(|| output.split_once("\n\n"))
        .unwrap_or(("", output));
    let user: Value =
        serde_json::from_str(body.trim()).map_err(|_| "GitHub didn't answer with an account.".to_string())?;
    if user["login"].as_str().is_none_or(str::is_empty) {
        return Err("GitHub didn't name an account for this token.".into());
    }
    let scopes = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("x-oauth-scopes").then(|| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
    });
    Ok((user, scopes))
}

/// The words for a token whose scopes lack what the agent and the runner use.
pub fn missing_scopes(scopes: &[String]) -> Option<String> {
    let missing: Vec<&str> = REQUIRED_SCOPES
        .into_iter()
        .filter(|need| !scopes.iter().any(|s| s == need))
        .collect();
    (!missing.is_empty()).then(|| {
        format!(
            "The token lacks the {} scope{}. Create a new one with Create a token: it fills the scopes in.",
            missing.join(" and "),
            if missing.len() == 1 { "" } else { "s" }
        )
    })
}

//! What the minute's fetch saw, told to the store (section 7's "tell the store what it sees"; `upsertHeaders`
//! in Retaskable.com's `agent/src/hostedBacklog.ts` and `agent/src/backlogLanded.ts`). After a good fetch of a
//! live project, paused or not: when `docs/specs/` changed on `origin/<default branch>`, its Items headers go to
//! the store as `upsert_from_specs` (backlog.md section 6.4), and the items whose merge commits reached the
//! default branch or production are sent as `landed` (section 6.5). Plain code: no model reads anything. A
//! failure is only logged; the next fetch tries again.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

use super::{Links, ProjectLoop};
use crate::backlog::Client;
use crate::config::DeployTarget;
use crate::spec_items::{SPECS_DIR, check_spec_items};
use crate::{lanes, smoke};

/// How long production is given to serve its branch's pushed head (`DEPLOY_WAIT_MS` in `backlogLanded.ts`).
const DEPLOY_WAIT: Duration = Duration::from_secs(20 * 60);

/// What this project's earlier reports saw: an unmoved branch or specs folder needs no new look.
#[derive(Debug, Default)]
pub(super) struct Seen {
    /// The repo's default branch on GitHub, asked of `gh` until it answers and then kept.
    pub(super) default_branch: Option<String>,
    /// The `docs/specs` tree the store last took (or turned down) headers from.
    sent_tree: Option<String>,
    /// The default branch's and the production branch's heads at the last good look.
    main: Option<String>,
    production: Option<String>,
    /// The commit production was last seen serving, from its `versionPath`.
    served: Option<String>,
    /// The production head pushed and not served yet, and since when it's been waited for.
    want: Option<(String, Instant)>,
    /// The last look at the landing went through, so nothing is owed until a branch moves.
    looked: bool,
}

/// An item the store says waits to land (`awaiting_landing`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Awaiting {
    id: String,
    status: String,
    #[serde(default)]
    merge_commit: Option<String>,
}

/// What a report needs to reach the store and the repo.
struct Ctx {
    client: Arc<Client>,
    repo: u64,
    token: String,
    repo_path: PathBuf,
    main: String,
}

impl Ctx {
    fn origin(&self, branch: &str) -> String {
        format!("origin/{branch}")
    }
}

impl ProjectLoop {
    /// Tells the store what the fetch just saw: the Items headers, then the landed items. Needs the repo id (learned
    /// on the first pass), the backlog link and the default branch; without one it waits for the next fetch.
    pub(super) async fn report(&mut self, links: &Links) {
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = links.keychain.get(&links.backlog_url) else {
            return;
        };
        if self.seen.default_branch.is_none() {
            self.seen.default_branch = default_branch(&self.project.github_repo).await;
        }
        let Some(main) = self.seen.default_branch.clone() else {
            tracing::warn!(
                "{}: not telling the store what the fetch saw: gh couldn't say the default branch",
                self.id
            );
            return;
        };
        let ctx = Ctx {
            client: self.client.clone(),
            repo,
            token,
            repo_path: PathBuf::from(&self.project.repo_path),
            main,
        };
        match upsert(&ctx, &mut self.seen).await {
            Ok(done) => tracing::debug!("{}: Items headers: {done}", self.id),
            Err(e) => tracing::warn!("{}: sending the Items headers failed: {e:#}", self.id),
        }
        match land(&ctx, &mut self.seen, self.project.deploy.production.as_ref()).await {
            Ok(done) => tracing::debug!("{}: landed items: {done}", self.id),
            Err(e) => tracing::warn!("{}: reporting landed items failed: {e:#}", self.id),
        }
    }
}

/// When `docs/specs/` on `origin/<default branch>` isn't the tree sent last, sends its Items headers as
/// `upsert_from_specs`, naming as `previous` the commit the store took headers from last. Not when the store's
/// commit isn't an ancestor of this one (another agent fetched later), nor when the headers hold a cycle; any other
/// problem leaves out only its own item. What it did, for the log.
async fn upsert(ctx: &Ctx, seen: &mut Seen) -> Result<String> {
    let at = ctx.origin(&ctx.main);
    let Some(commit) = git(
        &ctx.repo_path,
        &["rev-parse", "--verify", "--quiet", &format!("{at}^{{commit}}")],
    )
    .await
    else {
        return Ok(format!("no {at}"));
    };
    let Some(tree) = git(&ctx.repo_path, &["rev-parse", &format!("{at}:{SPECS_DIR}")]).await else {
        return Ok(format!("no {SPECS_DIR} on {at}"));
    };
    if seen.sent_tree.as_deref() == Some(tree.as_str()) {
        return Ok("unchanged".into());
    }
    let token = Some(ctx.token.as_str());
    let previous = ctx.client.specs_commit(ctx.repo, token).await?;
    if previous.as_deref() == Some(commit.as_str()) {
        seen.sent_tree = Some(tree);
        return Ok("already there".into());
    }
    if let Some(previous) = &previous
        && !contains(&ctx.repo_path, previous, &commit).await
    {
        // The store has a later commit's headers, or one this clone doesn't know: leave them.
        seen.sent_tree = Some(tree);
        return Ok(format!(
            "the store is at {}, not behind {}",
            short(previous),
            short(&commit)
        ));
    }
    let files = spec_files(&ctx.repo_path, &at).await.context("reading the specs")?;
    // A header may depend on an item the store has from elsewhere.
    let known: BTreeSet<String> = (ctx.client.list(ctx.repo, token).await?.into_iter())
        .map(|i| i.id)
        .collect();
    let (mut items, problems) = check_spec_items(&files, &known);
    // Only a cycle stops the headers (its items could never start); anything else is that item's alone.
    if let Some(cycle) = problems.iter().find(|p| p.is_cycle()) {
        seen.sent_tree = Some(tree);
        tracing::warn!(
            "not sending the Items headers at {}: {} line {}: {}",
            short(&commit),
            cycle.spec,
            cycle.line,
            cycle.message
        );
        return Ok("problems".into());
    }
    for p in &problems {
        tracing::warn!(
            "the Items headers at {}: {} line {}: {}; sent without it",
            short(&commit),
            p.spec,
            p.line,
            p.message
        );
    }
    // A dependency nothing names is left off.
    let named: BTreeSet<String> = items.iter().map(|i| i.id.clone()).collect();
    for item in &mut items {
        let id = item.id.clone();
        (item.depends_on).retain(|d| *d != id && (named.contains(d) || known.contains(d)));
    }
    #[derive(Deserialize)]
    struct Added {
        added: Vec<String>,
    }
    // No spec drafts are approved on this agent yet (spec-drafts), so none start `todo` and none has an asker.
    let args = json!({ "commit": commit, "previous": previous, "items": items, "approved": [], "askedBy": {} });
    let answer: Added = (ctx.client)
        .op(ctx.repo, token, "upsert_from_specs", args, "upsert answer")
        .await?;
    seen.sent_tree = Some(tree);
    tracing::info!(
        "sent the Items headers at {}: {} new",
        short(&commit),
        answer.added.len()
    );
    Ok("sent".into())
}

/// Sends `landed { where: "main" }` for each `on_branch` item whose merge commit the default branch now contains,
/// and `landed { where: "production" }` for each `on_main` one production contains. No production: on main is in
/// production. A production branch without a URL: its head stands in for what's deployed. With a URL: what
/// production was last seen serving, asked once a fetch for up to 20 minutes after its branch moves. Only when a
/// branch moved, production started serving it, or the last look failed. What it did, for the log.
async fn land(ctx: &Ctx, seen: &mut Seen, production: Option<&DeployTarget>) -> Result<String> {
    let Some(main_head) = head(ctx, &ctx.main).await else {
        return Ok(format!("no {}", ctx.origin(&ctx.main)));
    };
    let production = production.filter(|p| !p.branch.trim().is_empty());
    let version = production.and_then(version_url);
    let production_head = match production {
        Some(p) => head(ctx, p.branch.trim()).await,
        None => None,
    };
    let main_moved = seen.main.as_deref() != Some(main_head.as_str());
    let production_moved = production_head.is_some() && production_head != seen.production;
    if let (Some(head), Some(_), true) = (&production_head, &version, production_moved) {
        seen.want = Some((head.clone(), Instant::now()));
    }
    let served_now = match &version {
        Some(url) => served(ctx, seen, url).await,
        None => false,
    };
    if seen.looked && !main_moved && !production_moved && !served_now {
        return Ok("unchanged".into());
    }
    seen.looked = false;
    let token = Some(ctx.token.as_str());
    let awaiting: Vec<Awaiting> = (ctx.client)
        .op(ctx.repo, token, "awaiting_landing", json!({}), "items waiting to land")
        .await?;
    let on_branch = awaiting.iter().filter(|i| i.status == "on_branch");
    let on_main = send(ctx, "main", &main_head, contained(ctx, on_branch, &main_head).await).await?;
    let waiting = (awaiting.iter()).filter(|i| i.status == "on_main" || on_main.contains(&i.id));
    let deployed = match (production, &version) {
        (None, _) => Some(main_head.clone()),
        (Some(_), None) => production_head.clone(),
        (Some(_), Some(_)) => seen.served.clone(),
    };
    let done = match deployed {
        Some(deployed) => send(ctx, "production", &deployed, contained(ctx, waiting, &deployed).await).await?,
        None => Vec::new(),
    };
    seen.main = Some(main_head);
    if production_head.is_some() {
        seen.production = production_head;
    }
    seen.looked = true;
    Ok(format!("{} on main, {} in production", on_main.len(), done.len()))
}

/// One look at what production serves while its pushed head isn't served yet: whether it now serves that head
/// or a later commit. Past 20 minutes it stops asking until the branch moves again.
async fn served(ctx: &Ctx, seen: &mut Seen, url: &str) -> bool {
    let Some((head, since)) = seen.want.clone() else {
        return false;
    };
    if let Some(full) = served_sha(&ctx.repo_path, url).await
        && (full == head || contains(&ctx.repo_path, &head, &full).await)
    {
        seen.served = Some(full);
        seen.want = None;
        return true;
    }
    if since.elapsed() >= DEPLOY_WAIT {
        tracing::info!(
            "production didn't serve {} within 20 minutes; the next move of its branch tries again",
            short(&head)
        );
        seen.want = None;
    }
    false
}

/// The full sha production says it serves (`{ "sha": ... }` at `url`), or `None` while it doesn't say or names a
/// commit this clone doesn't know.
async fn served_sha(repo_path: &Path, url: &str) -> Option<String> {
    let answer = (reqwest::Client::new().get(url))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let sha = smoke::parse_deployed_sha(&answer.text().await.ok()?)?;
    git(
        repo_path,
        &["rev-parse", "--verify", "--quiet", &format!("{sha}^{{commit}}")],
    )
    .await
}

/// `<url><versionPath>` (`/version.json` by default) for production with a URL, else `None`.
fn version_url(target: &DeployTarget) -> Option<String> {
    let url = target.url.as_deref()?.trim().trim_end_matches('/');
    if url.is_empty() {
        return None;
    }
    let path = (target.version_path.as_deref().map(str::trim))
        .filter(|p| !p.is_empty())
        .unwrap_or("/version.json");
    Some(match path.starts_with('/') {
        true => format!("{url}{path}"),
        false => format!("{url}/{path}"),
    })
}

/// Sends `landed` for `items` (none: nothing is sent); the ids the store moved.
async fn send(ctx: &Ctx, place: &str, sha: &str, items: Vec<String>) -> Result<Vec<String>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let args = json!({ "where": place, "sha": sha, "items": items });
    let token = Some(ctx.token.as_str());
    let moved: Vec<String> = ctx.client.op(ctx.repo, token, "landed", args, "landed answer").await?;
    if !moved.is_empty() {
        let at = if place == "main" {
            "on the default branch at"
        } else {
            "in production at deploy"
        };
        tracing::info!("{} {at} {}", moved.join(", "), short(sha));
    }
    Ok(moved)
}

/// The ids of `items` whose merge commit `at` contains.
async fn contained(ctx: &Ctx, items: impl Iterator<Item = &Awaiting>, at: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for item in items {
        if let Some(commit) = &item.merge_commit
            && contains(&ctx.repo_path, commit, at).await
        {
            ids.push(item.id.clone());
        }
    }
    ids
}

/// Whether `at` has `commit` in its history; a commit this clone doesn't know isn't.
async fn contains(repo_path: &Path, commit: &str, at: &str) -> bool {
    git(repo_path, &["merge-base", "--is-ancestor", commit, at])
        .await
        .is_some()
}

async fn head(ctx: &Ctx, branch: &str) -> Option<String> {
    let at = format!("{}^{{commit}}", ctx.origin(branch));
    git(&ctx.repo_path, &["rev-parse", "--verify", "--quiet", &at]).await
}

/// The top-level specs on `at` (path and text): a split spec's Items header stays in its index (06.10-spec-layout.md).
async fn spec_files(repo_path: &Path, at: &str) -> Option<Vec<(String, String)>> {
    let listed = git(
        repo_path,
        &["ls-tree", "--name-only", at, "--", &format!("{SPECS_DIR}/")],
    )
    .await?;
    let mut files = Vec::new();
    for path in listed.lines().map(str::trim).filter(|l| l.ends_with(".md")) {
        let content = git_out(repo_path, &["show", &format!("{at}:{path}")]).await?;
        files.push((path.to_string(), content));
    }
    Some(files)
}

/// A git call in the project's repo, off the async threads: its trimmed output, or `None` when it failed.
async fn git(repo_path: &Path, args: &[&str]) -> Option<String> {
    Some(git_out(repo_path, args).await?.trim().to_string())
}

/// [`git`]'s output as git wrote it.
async fn git_out(repo_path: &Path, args: &[&str]) -> Option<String> {
    let repo_path = repo_path.to_path_buf();
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let out = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        lanes::git_output_in_worktree(&repo_path, &args)
    });
    out.await.ok()?.ok()
}

/// The repo's default branch on GitHub (`gh api repos/<repo> --jq .default_branch`).
async fn default_branch(github_repo: &str) -> Option<String> {
    if github_repo.is_empty() {
        return None;
    }
    let out = tokio::process::Command::new("gh")
        .args(["api", &format!("repos/{github_repo}"), "--jq", ".default_branch"])
        .output()
        .await
        .ok()?;
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !branch.is_empty()).then_some(branch)
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: Option<&str>, version_path: Option<&str>) -> DeployTarget {
        DeployTarget {
            branch: "main".into(),
            url: url.map(str::to_string),
            smoke: None,
            version_path: version_path.map(str::to_string),
        }
    }

    #[test]
    fn no_url_is_no_version_url() {
        assert_eq!(version_url(&target(None, None)), None);
    }

    #[test]
    fn a_blank_or_slash_only_url_is_no_version_url() {
        assert_eq!(version_url(&target(Some("   "), None)), None);
        assert_eq!(version_url(&target(Some("///"), None)), None);
    }

    #[test]
    fn the_default_path_joins_a_trimmed_url_with_no_double_slash() {
        assert_eq!(
            version_url(&target(Some("https://example.com/"), None)),
            Some("https://example.com/version.json".into())
        );
        assert_eq!(
            version_url(&target(Some("https://example.com"), None)),
            Some("https://example.com/version.json".into())
        );
    }

    #[test]
    fn a_custom_path_is_trimmed_and_joined() {
        assert_eq!(
            version_url(&target(Some("https://example.com"), Some(" /status.json "))),
            Some("https://example.com/status.json".into())
        );
    }

    #[test]
    fn a_custom_path_with_no_leading_slash_still_joins_with_one() {
        assert_eq!(
            version_url(&target(Some("https://example.com"), Some("status.json"))),
            Some("https://example.com/status.json".into())
        );
    }

    #[test]
    fn a_blank_path_falls_back_to_the_default() {
        assert_eq!(
            version_url(&target(Some("https://example.com"), Some("   "))),
            Some("https://example.com/version.json".into())
        );
    }
}

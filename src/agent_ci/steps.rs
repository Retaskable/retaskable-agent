//! docs/specs/agent-ci.md section 3.2: the three containers of one check, as Retaskable.com's `.agent-ci/runner.mjs`
//! runs them. The fetch holds the token and none of the change's code; the warm-up and the check hold no token.
//! Names, labels, volumes, environment and scripts here are the runner's, so one computer can run either.
use std::sync::OnceLock;

use regex::Regex;

/// The image the fetch and warm-up run in, and a check's when the repo names none (`.agent-ci/image`).
pub const DEFAULT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.63.0-noble";
/// Groups the step containers under `retaskable-ci` in Docker Desktop. Compose counts them as that project's
/// orphans, so never run its `up --remove-orphans` or `down` while a check is running.
pub const GROUP_LABEL: &str = "com.docker.compose.project=retaskable-ci";
const DEPS: &str = "agent-ci-deps:/deps";
const NPM: &str = "agent-ci-npm:/npm-cache";
const CARGO: &str = "agent-ci-cargo:/usr/local/cargo/registry";
/// The runner's own cache volumes, which `disk.rs` keeps under a size cap (never the mirror or the dependencies).
pub const CACHE_VOLUMES: [&str; 2] = ["agent-ci-npm", "agent-ci-cargo"];
/// The label every step container carries beside the group's, with this computer's slug as its value: what a cleanup
/// matches on so it never touches a container the runner did not make (docs/specs/docker.md section 10).
pub const RUNNER_LABEL_KEY: &str = "org.retaskable.ci-runner";

/// `org.retaskable.ci-runner=<computer>`.
pub fn runner_label(computer: &str) -> String {
    format!("{RUNNER_LABEL_KEY}={}", slug(computer))
}
/// Every traced command carries its clock time: a log shows where the minutes went.
const PS4: &str = "PS4='+ $(printf \"%(%T)T\" -1) '";
const IMAGE_MARK: &str = "AGENT_CI_IMAGE ";

/// Lowercase, with anything but `a-z0-9_-` turned into `-`.
pub fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `agent-ci-<computer>-<repo>--`: what only this computer's runner of this repo names its containers with.
pub fn name_prefix(computer: &str, repo: &str) -> String {
    format!("agent-ci-{}-{}--", slug(computer), slug(repo))
}

/// The three steps of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Fetch,
    Warm,
    Check,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Warm => "warm",
            Self::Check => "check",
        }
    }
}

/// What every step of one check shares.
pub struct Plan<'a> {
    pub computer: &'a str,
    /// `owner/repo`.
    pub repo: &'a str,
    pub sha: &'a str,
    /// `pull_request` or `push`.
    pub event: &'a str,
    pub pr: u64,
    pub cpus: u32,
}

impl Plan<'_> {
    pub fn name(&self, step: Step) -> String {
        let short: String = self.sha.chars().take(8).collect();
        format!(
            "{}{short}-{}-{}",
            name_prefix(self.computer, self.repo),
            self.event,
            step.as_str()
        )
    }

    /// The names of the three step containers, for the removal after the job.
    pub fn names(&self) -> Vec<String> {
        [Step::Fetch, Step::Warm, Step::Check].map(|s| self.name(s)).to_vec()
    }

    fn mirror(&self) -> String {
        format!("agent-ci-mirror-{}:/m", slug(self.repo))
    }

    /// The ref the fetch reads: the pull request's head, or main for a push.
    pub fn ref_name(&self) -> String {
        if self.event == "push" {
            "refs/heads/main".into()
        } else {
            format!("refs/pull/{}/head", self.pr)
        }
    }

    fn head(&self, step: Step) -> Vec<String> {
        ["run", "--rm", "--label", GROUP_LABEL, "--label"]
            .iter()
            .map(std::string::ToString::to_string)
            .chain([runner_label(self.computer), "--name".to_string(), self.name(step)])
            .collect()
    }

    /// The fetch's `docker run` arguments; the token is `-e T`, read from `docker`'s own environment.
    pub fn fetch_args(&self) -> Vec<String> {
        let mut a = self.head(Step::Fetch);
        a.extend(["-v".into(), self.mirror(), "-e".into(), "T".into()]);
        a.extend(["-e".into(), format!("REF={}", self.ref_name())]);
        a.extend(["-e".into(), format!("SHA={}", self.sha)]);
        a.extend([DEFAULT_IMAGE.into(), "sh".into(), "-c".into(), fetch_script(self.repo)]);
        a
    }

    /// The warm-up's: main's dependencies into the `agent-ci-deps` volume, from a clone of the mirror.
    pub fn warm_args(&self) -> Vec<String> {
        let mut a = self.head(Step::Warm);
        a.extend(["--cpus".into(), self.cpus.to_string()]);
        a.extend(["-v".into(), format!("{}:ro", self.mirror())]);
        a.extend(["-v".into(), DEPS.into(), "-v".into(), NPM.into()]);
        a.extend([
            "-e".into(),
            "CI=true".into(),
            "-e".into(),
            "npm_config_cache=/npm-cache".into(),
        ]);
        a.extend([DEFAULT_IMAGE.into(), "bash".into(), "-c".into(), warm_script()]);
        a
    }

    /// The check's. `files` is the pull request's changed files as JSON, `card` its card line (`None` when the
    /// body couldn't be read).
    pub fn check_args(&self, image: &str, files: &str, card: Option<&str>) -> Vec<String> {
        let mut a = self.head(Step::Check);
        a.extend(["--cpus".into(), self.cpus.to_string(), "--shm-size".into(), "1g".into()]);
        a.extend(["-v".into(), format!("{}:ro", self.mirror()), "-v".into(), NPM.into()]);
        a.extend(["-v".into(), format!("{DEPS}:ro"), "-v".into(), CARGO.into()]);
        let mut env = vec![
            "CI=true".to_string(),
            "npm_config_cache=/npm-cache".into(),
            "npm_config_prefer_offline=true".into(),
            format!("CI_PIPELINE_EVENT={}", self.event),
            format!("CI_PIPELINE_FILES={files}"),
            "CI_COMMIT_TARGET_BRANCH=main".into(),
            format!("SHA={}", self.sha),
            format!("CI_CARD_LINE={}", card.unwrap_or("")),
        ];
        if card.is_none() {
            env.push("CI_CARD_UNREADABLE=1".into());
        }
        for e in env {
            a.extend(["-e".into(), e]);
        }
        a.extend([image.into(), "bash".into(), "-c".into(), check_script(self.event)]);
        a
    }
}

/// The fetch: the repo's bare mirror volume is made once and brought up to date with main and the change's ref.
pub fn fetch_script(repo: &str) -> String {
    let url = format!("https://x-access-token:$T@github.com/{repo}.git");
    [
        "set -e".to_string(),
        "git config --global safe.directory \"*\"".into(),
        format!("[ -d /m/objects ] || git clone -q --bare \"{url}\" /m"),
        format!("git -C /m remote set-url origin \"{url}\""),
        // Two jobs can fetch at once: a ref lock is tried again.
        "ok=; for i in 1 2 3 4 5; do git -C /m fetch -q --no-tags origin \"+refs/heads/main:refs/heads/main\" \"+$REF:refs/ci/$SHA\" && ok=1 && break; sleep 3; done; [ -n \"$ok\" ] || exit 1".into(),
        // The image the repo wants for its checks, if it names one.
        format!("img=$(git -C /m show \"refs/ci/$SHA:.agent-ci/image\" 2>/dev/null | head -n 1 || true); [ -z \"$img\" ] || echo \"{IMAGE_MARK}$img\""),
    ]
    .join("\n")
}

/// The warm-up: the repo's own `.agent-ci/warm.sh`, run on main.
pub fn warm_script() -> String {
    [
        PS4,
        "set -ex",
        "git config --global safe.directory \"*\"",
        "git clone -q /m /w",
        "cd /w",
        "git checkout -q main",
        "bash .agent-ci/warm.sh",
    ]
    .join("\n")
}

/// The check: the change (a pull request merged into main) and the repo's own `.agent-ci/checks.sh`.
pub fn check_script(event: &str) -> String {
    let mut lines = vec![
        PS4,
        "set -ex",
        "git config --global safe.directory \"*\"",
        "git clone -q /m /w",
        "cd /w",
        "git fetch -q origin refs/ci/$SHA",
        "git checkout -q FETCH_HEAD",
    ];
    // A pull request is checked merged into main. A conflict fails at once: the head alone would fail on whatever
    // main has gained since, and say nothing of why.
    let merge = "git -c user.name=ci -c user.email=ci@localhost merge -q --no-edit origin/main || { echo \"error: conflicts with main; merge main into the branch\"; exit 1; }";
    if event != "push" {
        lines.push(merge);
    }
    let run = format!("bash .agent-ci/checks.sh {event}");
    lines.push(&run);
    lines.join("\n")
}

/// The image a fetch's log names (the line behind the marker), the default when it names none. A bad name is an
/// error before any docker call uses it.
pub fn image_from(log: &str) -> Result<String, String> {
    static VALID: OnceLock<Regex> = OnceLock::new();
    let valid = VALID.get_or_init(|| Regex::new(r"(?i)^[a-z0-9][a-z0-9._/:@-]*$").expect("a fixed pattern"));
    let log = log.replace('\r', "");
    let Some(line) = log.lines().find_map(|l| l.strip_prefix(IMAGE_MARK)).map(str::trim) else {
        return Ok(DEFAULT_IMAGE.into());
    };
    if line.is_empty() {
        return Ok(DEFAULT_IMAGE.into());
    }
    if !valid.is_match(line) || line.chars().count() > 200 {
        let shown: String = line.chars().take(80).collect();
        return Err(format!(".agent-ci/image is not an image name: {shown}"));
    }
    Ok(line.to_string())
}

#[cfg(test)]
mod tests;

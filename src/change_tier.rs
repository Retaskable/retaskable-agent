//! A change's tier (Retaskable.com docs/specs/simple-flow.md section 2.1; `shared/changeTier.ts` and
//! `shared/riskProfile.ts` there): how much a person has to be in the loop. Escalate-only, from deterministic
//! facts: the real diff's raise the plan's tier, never lower it. Pure; no LLM decides it. A signal that can't be
//! derived is never guessed at: it counts as the higher tier.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Routine,
    TryFirst,
    NeedsYou,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierResult {
    pub tier: Tier,
    pub reasons: Vec<String>,
}

/// The kinds of change a project's risk profile may set (`RISK_KINDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskKind {
    Pay,
    Del,
    Nav,
    Big,
    Flag,
    Screen,
    Feat,
    Db,
    Unk,
}

const KINDS: [(RiskKind, &str); 9] = [
    (RiskKind::Pay, "pay"),
    (RiskKind::Del, "del"),
    (RiskKind::Nav, "nav"),
    (RiskKind::Big, "big"),
    (RiskKind::Flag, "flag"),
    (RiskKind::Screen, "screen"),
    (RiskKind::Feat, "feat"),
    (RiskKind::Db, "db"),
    (RiskKind::Unk, "unk"),
];

/// Ship on its own, go live to the owner first, or wait for the owner's yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskChoice {
    Ship,
    Try,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Prototype,
    Early,
    Live,
}

fn stage_of(name: &str) -> Option<Stage> {
    match name {
        "prototype" => Some(Stage::Prototype),
        "early" => Some(Stage::Early),
        "live" => Some(Stage::Live),
        _ => None,
    }
}

fn choice_of(name: &str) -> Option<RiskChoice> {
    match name {
        "ship" => Some(RiskChoice::Ship),
        "try" => Some(RiskChoice::Try),
        "ask" => Some(RiskChoice::Ask),
        _ => None,
    }
}

/// What each stage does with each kind, before the owner changes anything (`DEFAULT_CHOICES`), in [`KINDS`] order:
/// pay, del, nav, big, flag, screen, feat, db, unk.
fn usual(stage: Stage) -> [RiskChoice; 9] {
    use RiskChoice::{Ask, Ship, Try};
    match stage {
        Stage::Prototype => [Ship, Try, Ship, Try, Try, Ship, Ship, Ship, Ship],
        Stage::Early => [Ask, Ask, Ask, Ask, Ask, Try, Try, Try, Try],
        Stage::Live => [Ask, Ask, Ask, Ask, Ask, Try, Try, Ask, Ask],
    }
}

/// A project's risk profile (`riskProfile` in the store's project settings): its stage and the owner's changes to
/// that stage's usual choices. Only the current stage's changes are kept, which is all `choicesOf` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskProfile {
    pub stage: Stage,
    changes: BTreeMap<RiskKind, RiskChoice>,
}

impl RiskProfile {
    /// The profile in a settings record's `values`; `None` when there is none or it can't be read (as no profile).
    pub fn from_values(values: &Value) -> Option<Self> {
        let profile = values.get("riskProfile")?;
        let name = profile.get("stage")?.as_str()?;
        let stage = stage_of(name)?;
        let mut changes = BTreeMap::new();
        if let Some(own) = profile
            .get("choices")
            .and_then(|c| c.get(name))
            .and_then(Value::as_object)
        {
            for (kind, key) in KINDS {
                if let Some(choice) = own.get(key).and_then(Value::as_str).and_then(choice_of) {
                    changes.insert(kind, choice);
                }
            }
        }
        Some(Self { stage, changes })
    }
}

/// What a project's risk profile says to do with each kind of change (`choicesOf`); no profile is Early's usual,
/// how it worked before profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices(BTreeMap<RiskKind, RiskChoice>);

impl Choices {
    pub fn of(profile: Option<&RiskProfile>) -> Self {
        let stage = profile.map_or(Stage::Early, |p| p.stage);
        let mut all: BTreeMap<_, _> = KINDS.iter().map(|(k, _)| *k).zip(usual(stage)).collect();
        if let Some(p) = profile {
            all.extend(p.changes.iter().map(|(k, c)| (*k, *c)));
        }
        Self(all)
    }

    /// The tier a kind of change takes: its choice, never above `cap`. Ship is routine, which adds no reason.
    fn tier(&self, kind: RiskKind, cap: Tier) -> Tier {
        let t = match self.0[&kind] {
            RiskChoice::Ship => Tier::Routine,
            RiskChoice::Try => Tier::TryFirst,
            RiskChoice::Ask => Tier::NeedsYou,
        };
        t.min(cap)
    }
}

/// The size of the piece a diff builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    S,
    M,
    L,
    Xl,
}

impl Size {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "S" => Some(Self::S),
            "M" => Some(Self::M),
            "L" => Some(Self::L),
            "XL" => Some(Self::Xl),
            _ => None,
        }
    }

    /// Most lines a piece of this size should change before the diff counts as bigger than the plan said.
    fn lines(self) -> u32 {
        match self {
            Self::S => 150,
            Self::M => 600,
            Self::L => 1500,
            Self::Xl => 4000,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::M => "M",
            Self::L => "L",
            Self::Xl => "XL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// The text this change adds, for a migration file (to see a DROP or ALTER); `None` when not read.
    pub added_text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSignals {
    pub files: Vec<DiffFile>,
    /// The change adds a dependency (package.json's dependencies grew).
    pub new_dependencies: bool,
    /// The size of the piece the diff builds; `None` when unknown.
    pub size: Option<Size>,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("a fixed pattern")
}

struct Rule {
    re: Regex,
    why: &'static str,
    kind: Option<RiskKind>,
}

fn rule(p: &str, why: &'static str, kind: Option<RiskKind>) -> Rule {
    Rule { re: re(p), why, kind }
}

static NAVIGATION: LazyLock<Rule> = LazyLock::new(|| {
    rule(
        r"navigation|(^|[/_.-])nav([/_.-]|$)|tab-?bar|main-?screen|home-?screen|onboarding|promise",
        "navigation or a main screen",
        Some(RiskKind::Nav),
    )
});
static TEST_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"\.test\.|test-render/"));
static MIGRATION_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"(^|/)migrations?/"));
static SCREEN_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"(^|/)app/src/.*\.tsx$"));
static ALTER_TABLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)\balter\s+table\s+\S+\s+(.*)"));
static DESTRUCTIVE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(drop|truncate|delete\s+from|rename)\b"));
static ADD_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^add\b"));
static CODE_NEEDS_YOU: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(r"(^|/)(auth|oauth)[^/]*\.|/auth/", "sign-in code", None),
        rule(r"payment|billing|stripe", "payments code", Some(RiskKind::Pay)),
        rule(
            r"secret|credential|\.env|wrangler\.toml",
            "secrets or deploy config",
            None,
        ),
        rule(
            r"(^|/)(agent/src/)?(commands|allowedcommands|hubcommands|projectcommand|mcpcommands)[^/]*\.ts$|agent/src/commands",
            "what runs on a computer",
            None,
        ),
    ]
});

/// An add-only migration (`ALTER TABLE x ADD COLUMN`, `CREATE ...`) is not destructive; anything else that alters a
/// table is.
fn destructive(sql: &str) -> bool {
    DESTRUCTIVE.is_match(sql)
        || ALTER_TABLE
            .captures_iter(sql)
            .any(|c| !ADD_WORD.is_match(c.get(1).map_or("", |m| m.as_str())))
}

/// Whether `path` (repo-relative) is a migration file.
pub fn is_migration(path: &str) -> bool {
    MIGRATION_PATH.is_match(&path.to_lowercase())
}

struct Tiering<'a> {
    choices: &'a Choices,
    tier: Tier,
    reasons: Vec<String>,
}

impl Tiering<'_> {
    fn up(&mut self, t: Tier, why: String) {
        self.tier = self.tier.max(t);
        if !self.reasons.contains(&why) {
            self.reasons.push(why);
        }
    }

    /// A kind the profile lets ship adds no reason and no tier; `cap` keeps a milder fact from reaching needsyou.
    fn by_kind(&mut self, kind: RiskKind, why: String, cap: Tier) {
        let t = self.choices.tier(kind, cap);
        if t != Tier::Routine {
            self.up(t, why);
        }
    }
}

/// The diff's own tier, from its real facts only (`diffTier`).
pub fn diff_tier(d: &DiffSignals, choices: &Choices) -> TierResult {
    let mut t = Tiering {
        choices,
        tier: Tier::Routine,
        reasons: Vec::new(),
    };
    for f in &d.files {
        let path = f.path.to_lowercase();
        let is_test = TEST_PATH.is_match(&path);
        if !is_test {
            for r in CODE_NEEDS_YOU.iter().filter(|r| r.re.is_match(&path)) {
                let why = format!("the change touches {} ({})", r.why, f.path);
                match r.kind {
                    Some(kind) => t.by_kind(kind, why, Tier::NeedsYou),
                    None => t.up(Tier::NeedsYou, why),
                }
            }
            // A screen the change touches is something a person should try first, as at plan time (tests aside).
            if SCREEN_PATH.is_match(&path) {
                t.by_kind(
                    RiskKind::Screen,
                    format!("a visible change ({})", f.path),
                    Tier::TryFirst,
                );
            }
            if NAVIGATION.re.is_match(&path) {
                let why = format!("the change touches {} ({})", NAVIGATION.why, f.path);
                t.by_kind(RiskKind::Nav, why, Tier::NeedsYou);
            }
        }
        if MIGRATION_PATH.is_match(&path) {
            // Unread migration text can't be shown safe, so it counts as destructive.
            if f.added_text.as_deref().is_none_or(destructive) {
                let why = format!("a migration that may drop or alter data ({})", f.path);
                t.by_kind(RiskKind::Del, why, Tier::NeedsYou);
            } else {
                t.by_kind(RiskKind::Db, format!("a migration ({})", f.path), Tier::TryFirst);
            }
        }
    }
    if d.new_dependencies {
        t.by_kind(RiskKind::Feat, "it adds a dependency".into(), Tier::TryFirst);
    }
    let lines: u32 = d.files.iter().map(|f| f.added + f.removed).sum();
    let limit = d.size.unwrap_or(Size::M).lines();
    match d.size {
        None if lines > limit => t.by_kind(
            RiskKind::Unk,
            format!("{lines} lines changed and the piece has no size"),
            Tier::TryFirst,
        ),
        Some(s) if lines > limit * 3 => t.by_kind(
            RiskKind::Big,
            format!("{lines} lines changed, far more than a {} piece", s.name()),
            Tier::NeedsYou,
        ),
        Some(s) if lines > limit => t.by_kind(
            RiskKind::Big,
            format!("{lines} lines changed, more than a {} piece", s.name()),
            Tier::TryFirst,
        ),
        _ => {}
    }
    TierResult {
        tier: t.tier,
        reasons: t.reasons,
    }
}

/// The final tier: the plan's, raised by the diff's, never lowered. The reasons are the plan's then the diff's that
/// raised it (`escalateTier`).
pub fn escalate_tier(plan: TierResult, diff: &DiffSignals, choices: &Choices) -> TierResult {
    let d = diff_tier(diff, choices);
    if d.tier <= plan.tier {
        return plan;
    }
    let reasons = plan.reasons.into_iter().chain(d.reasons).collect();
    TierResult { tier: d.tier, reasons }
}

#[cfg(test)]
#[path = "change_tier_tests.rs"]
mod tests;

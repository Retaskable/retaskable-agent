//! The settings the probes and the accessibility sweep share (`scan` in the project's config; `probesTarget` in
//! `scanChore.ts` and `a11yTarget` in `a11yChore.ts`): the staging url, never production, and the pages.
//! `ProjectConfig` has no `scan` section yet (`scan.probes`, `scan.a11y`, `scan.pages`, `scan.signedInPaths`):
//! wiring adds it and builds this from it.
use reqwest::Url;

use super::origin_of;
use crate::config::ProjectConfig;

/// `scan` and what it reads from the rest of the config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanSettings {
    /// `scan.probes`.
    pub probes: bool,
    /// `scan.a11y`.
    pub a11y: bool,
    /// `scan.pages`: empty means `lighthouse.pages`, then `/`.
    pub pages: Vec<String>,
    /// `scan.signedInPaths`: signed-in API paths read with the test token.
    pub signed_in_paths: Vec<String>,
    /// `lighthouse.pages`.
    pub lighthouse_pages: Vec<String>,
    /// `deploy.staging.url`, as saved.
    pub staging_url: Option<String>,
    /// `deploy.production.url`, as saved.
    pub production_url: Option<String>,
}

impl ScanSettings {
    /// The deploy urls and `lighthouse.pages` from a project's config; the `scan` flags are not in it yet.
    pub fn from_config(
        cfg: &ProjectConfig,
        probes: bool,
        a11y: bool,
        pages: Vec<String>,
        signed_in: Vec<String>,
    ) -> Self {
        ScanSettings {
            probes,
            a11y,
            pages,
            signed_in_paths: signed_in,
            lighthouse_pages: cfg.lighthouse.pages.clone(),
            staging_url: cfg.deploy.staging.as_ref().and_then(|s| s.url.clone()),
            production_url: cfg.deploy.production.as_ref().and_then(|s| s.url.clone()),
        }
    }

    /// The staging url's origin, or the note saying why the scan can't run there, in the scan's own words.
    pub fn staging_origin(&self, w: &Words) -> Result<(String, Url), String> {
        let Words { what, needs, runs } = *w;
        let raw = self.staging_url.as_deref().unwrap_or("").trim();
        if raw.is_empty() {
            return Err(format!("{what} {needs} a staging url (deploy.staging.url) to run on."));
        }
        let parsed = Url::parse(raw).ok();
        let Some((origin, url)) = parsed.and_then(|u| Some((origin_of(raw)?, u))) else {
            return Err(format!("{what} can't run on the staging url {raw}: it isn't a url."));
        };
        let prod = self.production_url.as_deref().unwrap_or("").trim();
        if !prod.is_empty() && origin_of(prod).is_some_and(|p| p == origin) {
            return Err(format!(
                "{what} {runs} only on staging, and the staging url is production's."
            ));
        }
        Ok((origin, url))
    }

    /// The pages to look at as paths: `scan.pages`, else `lighthouse.pages`, trimmed, each starting with `/`;
    /// empty when none are listed.
    pub fn listed_pages(&self) -> Vec<String> {
        let source = if self.pages.is_empty() {
            &self.lighthouse_pages
        } else {
            &self.pages
        };
        source
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(as_path)
            .collect()
    }
}

/// How a scan names itself in a note: "The probes need ... run", "The accessibility sweep needs ... runs".
#[derive(Debug, Clone, Copy)]
pub struct Words {
    pub what: &'static str,
    pub needs: &'static str,
    pub runs: &'static str,
}

pub const PROBES_WORDS: Words = Words {
    what: "The probes",
    needs: "need",
    runs: "run",
};

pub const A11Y_WORDS: Words = Words {
    what: "The accessibility sweep",
    needs: "needs",
    runs: "runs",
};

/// A path starting with `/` (`asPath`).
pub fn as_path(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

/// Whether the site is Retaskable's own (`retaskable.com` or a subdomain): its own routes and Sample are there.
pub fn is_retaskable(host: &str) -> bool {
    host == "retaskable.com" || host.ends_with(".retaskable.com")
}

#[cfg(test)]
mod tests;

//! The hosting providers' ids (`HostingId` in Retaskable.com's `shared/hosting.ts`): the only part of the provider
//! table the picking needs. A provider's name, steps and limits are the website's to show; the site looks them up by
//! these ids (Retaskable.com#3858).
use serde::{Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    GithubPages,
    CloudflarePages,
    Netlify,
    CloudflareWorkers,
    Render,
    CloudRun,
    CloudflareD1,
    Supabase,
    Neon,
    Turso,
    Itch,
    Eas,
    GithubReleases,
}

impl Id {
    pub const ALL: [Id; 13] = [
        Id::GithubPages,
        Id::CloudflarePages,
        Id::Netlify,
        Id::CloudflareWorkers,
        Id::Render,
        Id::CloudRun,
        Id::CloudflareD1,
        Id::Supabase,
        Id::Neon,
        Id::Turso,
        Id::Itch,
        Id::Eas,
        Id::GithubReleases,
    ];

    /// The id as the store and the site spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Id::GithubPages => "github-pages",
            Id::CloudflarePages => "cloudflare-pages",
            Id::Netlify => "netlify",
            Id::CloudflareWorkers => "cloudflare-workers",
            Id::Render => "render",
            Id::CloudRun => "cloud-run",
            Id::CloudflareD1 => "cloudflare-d1",
            Id::Supabase => "supabase",
            Id::Neon => "neon",
            Id::Turso => "turso",
            Id::Itch => "itch",
            Id::Eas => "eas",
            Id::GithubReleases => "github-releases",
        }
    }

    /// The provider with this id, or none (an exact match: `Object.hasOwn(HOSTING, id)`).
    pub fn parse(id: &str) -> Option<Id> {
        Id::ALL.into_iter().find(|i| i.as_str() == id)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

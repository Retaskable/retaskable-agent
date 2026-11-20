use serde::Deserialize;

/// A person's Start again (`retry`) or Carry on (`continue`) kept on the item (shared/backlogTypes.ts's `SendBack`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendBack {
    #[serde(default)]
    pub how: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pictures: Vec<String>,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub at: u64,
}

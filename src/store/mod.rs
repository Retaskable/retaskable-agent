//! On-disk state under `.retaskable/`: each project's `projects/<id>/state.json`, and this
//! computer's `machine.json` (agent-settings.md section 3; TypeScript's `MachineStore`).
mod flow;
mod kept_repo_id;
mod machine;
mod project;
mod smoke_state;

pub use flow::{BuiltTryout, FlowRecord, TryOut};
pub use kept_repo_id::saved_repo_id;
pub use machine::MachineStore;
pub use project::{RunRecord, Store};
pub use smoke_state::{SmokeState, SmokeStatus};

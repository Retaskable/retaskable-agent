//! Build this on an idea (docs/specs/agent/09.3.1-ideas.md): [`accept`] queues an idea the shared backlog already
//! holds as a spec draft and answers it `accept` there. This agent generates no ideas; the store's idea queue is the
//! store's own. `store` holds the `ideas_list` / `idea_answer` calls and the types they read.
pub(crate) mod accept;
mod store;

pub use accept::accept;
pub use store::{IdeaAnswer, IdeaState, Ideas, StoreIdea, StoreIdeaAnswer, StoreIdeaBody, StoreIdeas};

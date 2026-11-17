//! The commands on a project as a whole that its loop carries out (`unlink_project`): queued by [`crate::commands`] on the
//! connector's thread, taken by the project's loop on its next pass, which owns what they act on and answers them.
use std::sync::{Arc, Mutex};

use super::{Link, Reply};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCommand {
    /// **Unlink this project** (`unlink_project { removeLanes? }`): the loop refuses while anything runs, answers the command,
    /// gives back what this computer holds in the shared backlog, and takes the project out of the config (its start-paused
    /// marker too, and its lanes with `remove_lanes`). `done` is answered once that is over (`Ok`) or why it wasn't; the
    /// command's own thread then removes the state folder and restarts the agent.
    Unlink {
        project: String,
        command_id: String,
        remove_lanes: bool,
        done: Reply,
    },
    /// **Publish** or **Unpublish** the release note's public page (`release_publish`, `release_unpublish`): the loop, which
    /// has the store link, reads the note, asks the agent's own cloud through `link` and answers command `command_id`.
    Release {
        project: String,
        command_id: String,
        unpublish: bool,
        link: Link,
    },
}

impl ProjectCommand {
    fn project(&self) -> &str {
        match self {
            Self::Unlink { project, .. } | Self::Release { project, .. } => project,
        }
    }
}

/// The queue of [`ProjectCommand`]s.
#[derive(Debug, Clone, Default)]
pub struct ProjectCommands(Arc<Mutex<Vec<ProjectCommand>>>);

impl ProjectCommands {
    pub fn push(&self, command: ProjectCommand) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Removes and returns `project`'s queued commands, oldest first.
    pub fn take(&self, project: &str) -> Vec<ProjectCommand> {
        let mut queue = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project() == project);
        *queue = rest;
        mine
    }
}

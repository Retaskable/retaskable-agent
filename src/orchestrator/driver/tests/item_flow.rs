//! The item flow after the review, in the loop: CI, parking, the merge and its completion, try-outs and reopen. The steps are
//! scripted here (`Fake::done`); what they decide is `orchestrator/flow/`'s tests.
mod chain;
mod chore_nothing_left;
mod chores;
mod ci;
mod merge;
mod parking;
mod reopen;
mod tryouts;

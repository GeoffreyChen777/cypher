//! Agent sessions: the sessions engine and its run loop, forks, side chats,
//! the run journal, titles and scratch directories.

pub(crate) mod engine;
pub mod forks;
pub mod journal;
pub(crate) mod scratch;
pub(crate) mod side_chats;
pub(crate) mod title_settings;
pub(crate) mod titles;

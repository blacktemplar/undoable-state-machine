#![doc = include_str!("../README.md")]

mod error;
mod log;
mod session;
mod state_machine;
#[cfg(test)]
mod test_util;
mod transition;

pub use error::StateMachineError;
pub use log::{LogEntry, LogEntryKind, TransitionId};
pub use session::{RedoLastError, SessionHistory, UndoLastError};
pub use state_machine::StateMachine;
pub use transition::{Transition, TransitionError};

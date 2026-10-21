//! The error of the operations of a [`StateMachine`].

use std::{error::Error, fmt};

#[cfg(doc)]
use crate::StateMachine;
use crate::{TransitionError, TransitionId};

/// An error from [`StateMachine::apply`] or [`StateMachine::build`].
#[derive(Debug)]
pub enum StateMachineError {
    /// A transition returned an error
    Transition(TransitionId, TransitionError),
    /// An `Undo` or `Replace` targeted an id not present anywhere earlier in the log.
    UnknownTarget(TransitionId),
    /// An entry's id already appears earlier in the log. [`TransitionId`] must be unique, reusing
    /// one would make `Undo` and `Replace` targets ambiguous.
    DuplicateId(TransitionId),
    /// A `Replace` targeted an entry with the given id that is not an `Apply`, e.g. another
    /// `Replace` instead of the `Apply` that one replaces.
    ReplaceTargetNotApply(TransitionId),
    /// [`StateMachine::apply`] got a `Replace` whose target, the entry with the given id, is not in
    /// effect, i.e. undone.
    ReplaceTargetNotInEffect(TransitionId),
}

impl fmt::Display for StateMachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateMachineError::Transition(id, err) => {
                write!(f, "transition {id:?} failed: {err}")
            }
            StateMachineError::UnknownTarget(id) => {
                write!(f, "log entry targets unknown transition {id:?}")
            }
            StateMachineError::DuplicateId(id) => {
                write!(f, "transition id {id:?} already appears in the log")
            }
            StateMachineError::ReplaceTargetNotApply(id) => {
                write!(f, "replace targets {id:?}, which is not an apply entry")
            }
            StateMachineError::ReplaceTargetNotInEffect(id) => {
                write!(f, "replace targets {id:?}, which is not in effect")
            }
        }
    }
}

impl Error for StateMachineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StateMachineError::Transition(_, err) => Some(err.as_ref()),
            StateMachineError::UnknownTarget(_)
            | StateMachineError::DuplicateId(_)
            | StateMachineError::ReplaceTargetNotApply(_)
            | StateMachineError::ReplaceTargetNotInEffect(_) => None,
        }
    }
}

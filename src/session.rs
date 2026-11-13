//! Undo and redo of the changes of one session to a [`StateMachine`] that other sessions append
//! to as well.

#[cfg(test)]
mod tests;

use std::{error::Error, fmt};

use crate::{LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionId};

/// What one session did to a [`StateMachine`], so that it can undo and redo its own changes in
/// the order it made them, as an editor does, while other sessions append to the same log, e.g.
/// through a sync.
///
/// Every entry the session appends through [`Self::apply`] is one of its actions, whatever its
/// kind: an undo targeting an entry of its choice, such as the change of another session, or a
/// replacement is an action as well. [`Self::undo_last`] undoes the latest action of the session
/// and [`Self::redo_last`] redoes the action it undid last, each by appending a
/// [`LogEntryKind::Undo`] naming its target, so that the log means the same however it is merged
/// with the logs of other sessions. A new action leaves nothing to redo; the entries of other
/// sessions change neither what there is to undo nor to redo.
///
/// Only an entry of another session can make undoing or redoing fail: one that undid the action to
/// undo, or the undo of the action to redo, or one that does not apply without the action, or with
/// it. Then the history ends there: a failed undo leaves nothing to undo, a failed redo nothing to
/// redo, rather than passing over the action. [`Self::next_undo`] and [`Self::next_redo`] tell
/// beforehand what has been undone or redone elsewhere.
///
/// The history holds the ids of the entries alone, and is not part of the log: a log restored
/// from somewhere else comes with no history, and a session ends with its history.
///
/// # Examples
///
/// ```
/// use undoable_state_machine::{SessionHistory, StateMachine, Transition, TransitionError};
///
/// struct Add(i64);
///
/// impl Transition<i64> for Add {
///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
///         Ok(())
///     }
///
///     fn apply(&self, state: &mut i64) {
///         *state += self.0;
///     }
/// }
///
/// let mut sm = StateMachine::<i64, Add>::new();
/// let mut history = SessionHistory::new();
/// history.apply_new(&mut sm, Add(5))?;
/// // a change of another session, e.g. pulled in by a sync
/// sm.apply_new(Add(100))?;
///
/// // undoes the latest change of this session, not the latest one in the log
/// history.undo_last(&mut sm)?;
/// assert_eq!(*sm.current(), 100);
/// history.redo_last(&mut sm)?;
/// assert_eq!(*sm.current(), 105);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default, Clone)]
pub struct SessionHistory {
    /// The actions of the session that [`Self::undo_last`] has not undone, oldest first.
    done: Vec<TransitionId>,
    /// The actions [`Self::undo_last`] undid since the latest action, each with the undo entry
    /// appended for it, the one undone last at the end.
    undone: Vec<(TransitionId, TransitionId)>,
}

impl SessionHistory {
    /// A history of a session that has done nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `entry` to `machine`, see [`StateMachine::apply`], as an action of this session,
    /// which leaves nothing to redo.
    ///
    /// # Errors
    /// Returns the error of [`StateMachine::apply`], in which case neither the log nor the history
    /// changes.
    pub fn apply<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
        entry: LogEntry<T>,
    ) -> Result<(), StateMachineError> {
        let id = entry.id();
        machine.apply(entry)?;
        self.done.push(id);
        self.undone.clear();
        Ok(())
    }

    /// Applies `transition` to `machine` as a new [`LogEntryKind::Apply`] with a fresh
    /// [`TransitionId`], see [`StateMachine::apply_new`], as an action of this session, which
    /// leaves nothing to redo. Returns the id of the entry.
    ///
    /// # Errors
    /// Returns the error of [`StateMachine::apply_new`], in which case neither the log nor the
    /// history changes.
    pub fn apply_new<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
        transition: T,
    ) -> Result<TransitionId, StateMachineError> {
        let id = TransitionId::new();
        self.apply(machine, LogEntry::new(id, LogEntryKind::Apply(transition)))?;
        Ok(id)
    }

    /// The id of the entry of the action [`Self::undo_last`] undoes next, unless another session
    /// has undone it already. `None` if there is nothing to undo.
    ///
    /// The undo may still be rejected, if an entry in effect does not apply without the action:
    /// finding that out replays the log, as undoing does.
    #[must_use]
    pub fn next_undo<S, T>(&self, machine: &StateMachine<S, T>) -> Option<TransitionId> {
        // an action of the session undone by another action of the session lies below that one,
        // which is undone first, so the latest action is only out of effect if another session
        // undid it
        let &action = self.done.last()?;
        (machine.is_in_effect(action) == Some(true)).then_some(action)
    }

    /// The id of the entry of the action [`Self::redo_last`] redoes next, unless another session
    /// has redone it already. `None` if there is nothing to redo.
    ///
    /// The redo may still be rejected, if the action does not apply together with the entries in
    /// effect.
    #[must_use]
    pub fn next_redo<S, T>(&self, machine: &StateMachine<S, T>) -> Option<TransitionId> {
        let &(action, undo) = self.undone.last()?;
        (machine.is_in_effect(undo) == Some(true)).then_some(action)
    }

    /// Undoes the latest action of this session that [`Self::undo_last`] has not undone, and
    /// returns the id of the undo entry appended for that.
    ///
    /// As long as only this session appends to the log, undoing its actions one after another
    /// returns to the states the log was in before them, which cannot fail.
    ///
    /// # Errors
    /// Returns [`UndoLastError::NothingToUndo`] if the session has no such action,
    /// [`UndoLastError::UndoneElsewhere`] if another session has undone it, and
    /// [`UndoLastError::Rejected`] if an entry in effect does not apply without it, see
    /// [`StateMachine::apply`]. The log does not change, and in the last two cases the session is
    /// left with nothing to undo; what it can redo stays.
    pub fn undo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, UndoLastError> {
        if self.done.is_empty() {
            return Err(UndoLastError::NothingToUndo);
        }
        let Some(action) = self.next_undo(machine) else {
            self.done.clear();
            return Err(UndoLastError::UndoneElsewhere);
        };
        let id = TransitionId::new();
        if let Err(error) = machine.apply(LogEntry::new(id, LogEntryKind::Undo(action))) {
            self.done.clear();
            return Err(UndoLastError::Rejected(error));
        }
        self.done.pop();
        self.undone.push((action, id));
        Ok(id)
    }

    /// Redoes the action of this session that [`Self::undo_last`] undid last, as long as the
    /// session took no new action since, by undoing that undo, and returns the id of the entry
    /// appended for that.
    ///
    /// # Errors
    /// Returns [`RedoLastError::NothingToRedo`] if there is no such action,
    /// [`RedoLastError::RedoneElsewhere`] if another session has undone the undo already, and
    /// [`RedoLastError::Rejected`] if the action does not apply together with the entries in
    /// effect, see [`StateMachine::apply`]. The log does not change, and in the last two cases the
    /// session is left with nothing to redo; what it can undo stays.
    pub fn redo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, RedoLastError> {
        let &(action, undo) = self.undone.last().ok_or(RedoLastError::NothingToRedo)?;
        if machine.is_in_effect(undo) != Some(true) {
            self.undone.clear();
            return Err(RedoLastError::RedoneElsewhere);
        }
        let id = TransitionId::new();
        if let Err(error) = machine.apply(LogEntry::new(id, LogEntryKind::Undo(undo))) {
            self.undone.clear();
            return Err(RedoLastError::Rejected(error));
        }
        self.undone.pop();
        self.done.push(action);
        Ok(id)
    }
}

/// An error from [`SessionHistory::undo_last`].
#[derive(Debug)]
pub enum UndoLastError {
    /// The session has no action that is not undone.
    NothingToUndo,
    /// Another session has undone the action to undo, so the history no longer matches the log.
    UndoneElsewhere,
    /// Undoing the action was rejected, because an entry in effect does not apply without it.
    Rejected(StateMachineError),
}

impl From<StateMachineError> for UndoLastError {
    fn from(error: StateMachineError) -> Self {
        Self::Rejected(error)
    }
}

impl fmt::Display for UndoLastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UndoLastError::NothingToUndo => f.write_str("there is nothing to undo"),
            UndoLastError::UndoneElsewhere => {
                f.write_str("the change to undo was undone by another session already")
            }
            UndoLastError::Rejected(error) => error.fmt(f),
        }
    }
}

impl Error for UndoLastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            UndoLastError::NothingToUndo | UndoLastError::UndoneElsewhere => None,
            UndoLastError::Rejected(error) => error.source(),
        }
    }
}

/// An error from [`SessionHistory::redo_last`].
#[derive(Debug)]
pub enum RedoLastError {
    /// The session undid no action since its latest one.
    NothingToRedo,
    /// Another session has undone the undo of the action to redo, so the history no longer
    /// matches the log.
    RedoneElsewhere,
    /// Redoing the action was rejected, because it does not apply together with the entries in
    /// effect.
    Rejected(StateMachineError),
}

impl From<StateMachineError> for RedoLastError {
    fn from(error: StateMachineError) -> Self {
        Self::Rejected(error)
    }
}

impl fmt::Display for RedoLastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RedoLastError::NothingToRedo => f.write_str("there is nothing to redo"),
            RedoLastError::RedoneElsewhere => {
                f.write_str("the change to redo was redone by another session already")
            }
            RedoLastError::Rejected(error) => error.fmt(f),
        }
    }
}

impl Error for RedoLastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RedoLastError::NothingToRedo | RedoLastError::RedoneElsewhere => None,
            RedoLastError::Rejected(error) => error.source(),
        }
    }
}

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
/// replacement is an action as well. [`Self::apply_all`] appends several entries as one action,
/// which is undone and redone as a whole. [`Self::undo_last`] undoes the latest action of the
/// session and [`Self::redo_last`] redoes the action it undid last, each by appending a
/// [`LogEntryKind::Undo`] per entry of the action naming that entry as its target, so that the log
/// means the same however it is merged with the logs of other sessions. A new action leaves
/// nothing to redo; the entries of other sessions change neither what there is to undo nor to
/// redo.
///
/// Only an entry of another session can make undoing or redoing fail: one that undid an entry of
/// the action to undo, or one of the undos of the action to redo, or one that does not apply
/// without the action, or with it. Then the history ends there: a failed undo leaves nothing to
/// undo, a failed redo nothing to redo, rather than passing over the action. [`Self::next_undo`]
/// and [`Self::next_redo`] tell beforehand what has been undone or redone elsewhere.
///
/// The history holds the ids of the entries alone, and is not part of the log: a log restored
/// from somewhere else comes with no history, and a session ends with its history. Which entries
/// form one action is not in the log either.
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
/// history.apply_new(&mut sm, Add(5), ())?;
/// // a change of another session, e.g. pulled in by a sync
/// sm.apply_new(Add(100), ())?;
///
/// // undoes the latest change of this session, not the latest one in the log
/// history.undo_last(&mut sm, ())?;
/// assert_eq!(*sm.current(), 100);
/// history.redo_last(&mut sm, ())?;
/// assert_eq!(*sm.current(), 105);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default, Clone)]
pub struct SessionHistory {
    /// The actions of the session that [`Self::undo_last`] has not undone, oldest first, each as
    /// the ids of its entries, oldest first, never none.
    done: Vec<Vec<TransitionId>>,
    /// The actions [`Self::undo_last`] undid since the latest action, the one undone last at the
    /// end.
    undone: Vec<UndoneAction>,
}

/// An action of a [`SessionHistory`] that [`SessionHistory::undo_last`] undid.
#[derive(Debug, Clone)]
struct UndoneAction {
    /// The ids of the entries of the action, oldest first, never none.
    entries: Vec<TransitionId>,
    /// The ids of the undo entries appended for them, in the order appended, i.e. the undo of the
    /// newest entry first.
    undos: Vec<TransitionId>,
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
    pub fn apply<S: Default, T: Transition<S>, M>(
        &mut self,
        machine: &mut StateMachine<S, T, M>,
        entry: LogEntry<T, M>,
    ) -> Result<(), StateMachineError> {
        let id = entry.id();
        machine.apply(entry)?;
        self.push_action(vec![id]);
        Ok(())
    }

    /// Applies `entries` to `machine` in order, see [`StateMachine::apply`], as **one** action of
    /// this session, which leaves nothing to redo: [`Self::undo_last`] undoes them together, and
    /// [`Self::redo_last`] redoes them together. Without entries, nothing happens, and there is
    /// no action.
    ///
    /// # Errors
    /// Returns the error of [`StateMachine::apply`] for the first entry it rejects. Then the
    /// entries applied before that one are undone again, by appending an undo of each, newest
    /// first, with the meta of the rejected entry, so that the state is as before and the
    /// history unchanged; the log keeps the entries and their undos. The entries after the
    /// rejected one are not applied.
    ///
    /// # Panics
    /// Panics if undoing an entry applied before the rejected one is rejected, which cannot
    /// happen as long as [`Transition::apply`] is deterministic, as it must be: every undo
    /// returns to a state the log was in before.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, SessionHistory, StateMachine, Transition, TransitionError,
    ///     TransitionId,
    /// };
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
    /// history.apply_all(
    ///     &mut sm,
    ///     [1, 2, 3].map(|n| LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Add(n)), ())),
    /// )?;
    /// assert_eq!(*sm.current(), 6);
    ///
    /// assert_eq!(history.undo_last(&mut sm, ())?.len(), 3);
    /// assert_eq!(*sm.current(), 0);
    /// history.redo_last(&mut sm, ())?;
    /// assert_eq!(*sm.current(), 6);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn apply_all<S: Default, T: Transition<S>, M: Clone>(
        &mut self,
        machine: &mut StateMachine<S, T, M>,
        entries: impl IntoIterator<Item = LogEntry<T, M>>,
    ) -> Result<(), StateMachineError> {
        let mut applied = Vec::new();
        for entry in entries {
            let id = entry.id();
            let meta = entry.meta().clone();
            if let Err(error) = machine.apply(entry) {
                for &id in applied.iter().rev() {
                    machine
                        .apply(LogEntry::new(
                            TransitionId::new(),
                            LogEntryKind::Undo(id),
                            meta.clone(),
                        ))
                        .expect("undoing the latest entry in effect returns to an earlier state");
                }
                return Err(error);
            }
            applied.push(id);
        }
        if !applied.is_empty() {
            self.push_action(applied);
        }
        Ok(())
    }

    /// Applies `transition` to `machine` as a new [`LogEntryKind::Apply`] with a fresh
    /// [`TransitionId`] and `meta`, see [`StateMachine::apply_new`], as an action of this
    /// session, which leaves nothing to redo. Returns the id of the entry.
    ///
    /// # Errors
    /// Returns the error of [`StateMachine::apply_new`], in which case neither the log nor the
    /// history changes.
    pub fn apply_new<S: Default, T: Transition<S>, M>(
        &mut self,
        machine: &mut StateMachine<S, T, M>,
        transition: T,
        meta: M,
    ) -> Result<TransitionId, StateMachineError> {
        let id = TransitionId::new();
        self.apply(
            machine,
            LogEntry::new(id, LogEntryKind::Apply(transition), meta),
        )?;
        Ok(id)
    }

    /// Records the entries with the ids `entries`, just appended, as an action.
    fn push_action(&mut self, entries: Vec<TransitionId>) {
        self.done.push(entries);
        self.undone.clear();
    }

    /// The ids of the entries of the action [`Self::undo_last`] undoes next, oldest first, unless
    /// another session has undone any of them already. `None` if there is nothing to undo.
    ///
    /// The undo may still be rejected, if an entry in effect does not apply without the action:
    /// finding that out replays the log, as undoing does.
    #[must_use]
    pub fn next_undo<S, T, M>(&self, machine: &StateMachine<S, T, M>) -> Option<&[TransitionId]> {
        let action = self.done.last()?;
        is_undoable(action, machine).then_some(action.as_slice())
    }

    /// The ids of the entries of the action [`Self::redo_last`] redoes next, oldest first, unless
    /// another session has redone any of them already. `None` if there is nothing to redo.
    ///
    /// The redo may still be rejected, if the action does not apply together with the entries in
    /// effect.
    #[must_use]
    pub fn next_redo<S, T, M>(&self, machine: &StateMachine<S, T, M>) -> Option<&[TransitionId]> {
        let action = self.undone.last()?;
        is_redoable(action, machine).then_some(action.entries.as_slice())
    }

    /// Undoes the latest action of this session that [`Self::undo_last`] has not undone, by
    /// appending an undo of each of its entries, newest first, each with `meta`, and returns the
    /// ids of the undo entries appended, in that order.
    ///
    /// As long as only this session appends to the log, undoing its actions one after another
    /// returns to the states the log was in before them, which cannot fail.
    ///
    /// # Errors
    /// Returns [`UndoLastError::NothingToUndo`] if the session has no such action,
    /// [`UndoLastError::UndoneElsewhere`] if another session has undone any of its entries, see
    /// [`Self::next_undo`], and [`UndoLastError::Rejected`] if an entry in effect does not apply
    /// without one of them, see [`StateMachine::apply`]. In the last two cases the session is
    /// left with nothing to undo; what it can redo stays. The log does not change, except when an
    /// undo is rejected part of the way through an action of several entries: the undos appended
    /// before stay in the log, so those entries stay undone, and the action cannot be redone
    /// through the history.
    pub fn undo_last<S: Default, T: Transition<S>, M: Clone>(
        &mut self,
        machine: &mut StateMachine<S, T, M>,
        meta: M,
    ) -> Result<Vec<TransitionId>, UndoLastError> {
        let entries = self.done.pop().ok_or(UndoLastError::NothingToUndo)?;
        if !is_undoable(&entries, machine) {
            self.done.clear();
            return Err(UndoLastError::UndoneElsewhere);
        }
        let mut undos = Vec::with_capacity(entries.len());
        for &entry in entries.iter().rev() {
            let id = TransitionId::new();
            let undo = LogEntry::new(id, LogEntryKind::Undo(entry), meta.clone());
            if let Err(error) = machine.apply(undo) {
                self.done.clear();
                return Err(UndoLastError::Rejected(error));
            }
            undos.push(id);
        }
        self.undone.push(UndoneAction {
            entries,
            undos: undos.clone(),
        });
        Ok(undos)
    }

    /// Redoes the action of this session that [`Self::undo_last`] undid last, as long as the
    /// session took no new action since, by undoing the undo of each of its entries, oldest
    /// first, each undo with `meta`, and returns the ids of the entries appended for that, in
    /// that order.
    ///
    /// # Errors
    /// Returns [`RedoLastError::NothingToRedo`] if there is no such action,
    /// [`RedoLastError::RedoneElsewhere`] if another session has undone any of the undos already,
    /// see [`Self::next_redo`], and [`RedoLastError::Rejected`] if an entry of the action does
    /// not apply together with the entries in effect, see [`StateMachine::apply`]. In the last
    /// two cases the session is left with nothing to redo; what it can undo stays. The log does
    /// not change, except when a redo is rejected part of the way through an action of several
    /// entries: the entries appended before stay in the log, so the entries they redid stay in
    /// effect, and the action cannot be undone through the history.
    pub fn redo_last<S: Default, T: Transition<S>, M: Clone>(
        &mut self,
        machine: &mut StateMachine<S, T, M>,
        meta: M,
    ) -> Result<Vec<TransitionId>, RedoLastError> {
        let action = self.undone.pop().ok_or(RedoLastError::NothingToRedo)?;
        if !is_redoable(&action, machine) {
            self.undone.clear();
            return Err(RedoLastError::RedoneElsewhere);
        }
        let mut appended = Vec::with_capacity(action.undos.len());
        for &undo in action.undos.iter().rev() {
            let id = TransitionId::new();
            let entry = LogEntry::new(id, LogEntryKind::Undo(undo), meta.clone());
            if let Err(error) = machine.apply(entry) {
                self.undone.clear();
                return Err(RedoLastError::Rejected(error));
            }
            appended.push(id);
        }
        self.done.push(action.entries);
        Ok(appended)
    }
}

/// Whether no other session has undone any of `entries`, the ids of the entries of the latest
/// action of a session that it has not undone, oldest first.
fn is_undoable<S, T, M>(entries: &[TransitionId], machine: &StateMachine<S, T, M>) -> bool {
    // an action of the session undone by a later action of the session lies below that one, which
    // is undone first, and an entry undone by a later entry of the same action is in effect again
    // once undoing the action reaches it, so an entry is only out of effect for good if another
    // session undid it
    entries.iter().enumerate().all(|(index, &id)| {
        machine.is_in_effect(id) == Some(true)
            || entries[index + 1..].iter().any(|&later| {
                matches!(
                    machine.entry(later).map(LogEntry::kind),
                    Some(&LogEntryKind::Undo(target)) if target == id
                )
            })
    })
}

/// Whether no other session has undone any of the undos of `action`.
fn is_redoable<S, T, M>(action: &UndoneAction, machine: &StateMachine<S, T, M>) -> bool {
    action
        .undos
        .iter()
        .all(|&undo| machine.is_in_effect(undo) == Some(true))
}

/// An error from [`SessionHistory::undo_last`].
#[derive(Debug)]
pub enum UndoLastError {
    /// The session has no action that is not undone.
    NothingToUndo,
    /// Another session has undone an entry of the action to undo, so the history no longer
    /// matches the log.
    UndoneElsewhere,
    /// Undoing the action was rejected, because an entry in effect does not apply without it, or
    /// without one of its entries.
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
    /// Another session has undone an undo of the action to redo, so the history no longer matches
    /// the log.
    RedoneElsewhere,
    /// Redoing the action was rejected, because one of its entries does not apply together with
    /// the entries in effect.
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

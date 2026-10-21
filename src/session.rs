//! Undo and redo of the changes of one session to a [`StateMachine`] that other sessions append
//! to as well.

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
/// with the logs of other sessions. A new action leaves nothing to redo. If another session undid
/// the action to undo, or the undo of the action to redo, the history no longer matches the log,
/// and undoing or redoing fails rather than passing over it.
///
/// The history holds the ids of the entries alone, and is not part of the log: a log restored
/// from somewhere else comes with no history, and a session ends with its history.
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
///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
///         *state += self.0;
///         Ok(())
///     }
/// }
///
/// let mut sm = StateMachine::<i64, Add>::new();
/// let mut history = SessionHistory::new();
/// history.apply(
///     &mut sm,
///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Add(5))),
/// )?;
/// // a change of another session, e.g. pulled in by a sync
/// sm.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Apply(Add(100)),
/// ))?;
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

    /// Undoes the latest action of this session that [`Self::undo_last`] has not undone, and
    /// returns the id of the undo entry appended for that.
    ///
    /// As long as only this session appends to the log, undoing its actions one after another
    /// returns to the states the log was in before them, which cannot fail. Only an entry another
    /// session appended can make it fail: one that undid the action, or one that depends on it.
    ///
    /// # Errors
    /// Returns [`UndoLastError::NothingToUndo`] if the session has no such action,
    /// [`UndoLastError::UndoneElsewhere`] if another session has undone it, and
    /// [`UndoLastError::Rejected`] if an entry in effect does not apply without it, see
    /// [`StateMachine::apply`]. In each case neither the log nor the history changes, so the
    /// action stays the next one to undo.
    pub fn undo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, UndoLastError> {
        let &action = self.done.last().ok_or(UndoLastError::NothingToUndo)?;
        // an action of the session undone by another action of the session lies below that one,
        // which is undone first, so the latest action is only out of effect if another session
        // undid it
        if machine.is_in_effect(action) != Some(true) {
            return Err(UndoLastError::UndoneElsewhere);
        }
        let id = TransitionId::new();
        machine.apply(LogEntry::new(id, LogEntryKind::Undo(action)))?;
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
    /// effect, see [`StateMachine::apply`]. In each case neither the log nor the history changes,
    /// so the action stays the next one to redo.
    pub fn redo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, RedoLastError> {
        let &(action, undo) = self.undone.last().ok_or(RedoLastError::NothingToRedo)?;
        if machine.is_in_effect(undo) != Some(true) {
            return Err(RedoLastError::RedoneElsewhere);
        }
        let id = TransitionId::new();
        machine.apply(LogEntry::new(id, LogEntryKind::Undo(undo)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        I64Transition, RequireAtLeast, StringTransition, apply_entry, apply_get_id, push_entry,
        replace_push_entry,
    };

    /// Pushes `c` as an action of `history`, and returns the id of its entry.
    fn push_in(
        history: &mut SessionHistory,
        sm: &mut StateMachine<String, StringTransition>,
        c: char,
    ) -> TransitionId {
        let entry = push_entry(c);
        let id = entry.id();
        history.apply(sm, entry).unwrap();
        id
    }

    #[test]
    fn a_session_undoes_and_redoes_its_actions_in_order() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        for c in ['a', 'b', 'c'] {
            push_in(&mut history, &mut sm, c);
        }

        history.undo_last(&mut sm).unwrap();
        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "a");
        history.redo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "ab");
        history.undo_last(&mut sm).unwrap();
        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "");

        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::NothingToUndo)
        ));
        for expected in ["a", "ab", "abc"] {
            history.redo_last(&mut sm).unwrap();
            assert_eq!(sm.current(), expected);
        }
        assert!(matches!(
            history.redo_last(&mut sm),
            Err(RedoLastError::NothingToRedo)
        ));
    }

    #[test]
    fn a_new_action_leaves_nothing_to_redo() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        history.undo_last(&mut sm).unwrap();

        push_in(&mut history, &mut sm, 'b');

        assert!(matches!(
            history.redo_last(&mut sm),
            Err(RedoLastError::NothingToRedo)
        ));
        assert_eq!(sm.current(), "b");
    }

    #[test]
    fn a_session_undoes_its_own_actions_rather_than_those_of_another_session() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        sm.apply(push_entry('b')).unwrap();

        history.undo_last(&mut sm).unwrap();

        assert_eq!(sm.current(), "b");
        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::NothingToUndo)
        ));
    }

    /// An undo targeting an entry of the session's choice is an action like any other, so undoing
    /// it brings back what it undid.
    #[test]
    fn undoing_a_targeted_undo_brings_back_what_it_undid() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        let a = push_in(&mut history, &mut sm, 'a');
        push_in(&mut history, &mut sm, 'b');
        history
            .apply(
                &mut sm,
                LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a)),
            )
            .unwrap();
        assert_eq!(sm.current(), "b");

        history.undo_last(&mut sm).unwrap();

        assert_eq!(sm.current(), "ab");
        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "a");
    }

    #[test]
    fn undoing_a_replacement_brings_back_the_replaced_transition() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        let a = push_in(&mut history, &mut sm, 'a');
        push_in(&mut history, &mut sm, 'b');
        history.apply(&mut sm, replace_push_entry(a, 'x')).unwrap();
        assert_eq!(sm.current(), "xb");

        history.undo_last(&mut sm).unwrap();

        assert_eq!(sm.current(), "ab");
    }

    /// An action another session undid is not passed over, which would undo an older action while
    /// a newer one could still come back, e.g. if the other session undoes its undo.
    #[test]
    fn an_action_another_session_undid_stops_undoing() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        let b = push_in(&mut history, &mut sm, 'b');
        let elsewhere = apply_get_id(
            &mut sm,
            LogEntry::new(TransitionId::new(), LogEntryKind::Undo(b)),
        );
        let log_len = sm.log().len();

        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::UndoneElsewhere)
        ));
        assert_eq!(sm.log().len(), log_len);
        assert_eq!(sm.current(), "a");

        // once the other session takes its undo back, the action is the one undone next
        sm.apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Undo(elsewhere),
        ))
        .unwrap();
        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "a");
    }

    #[test]
    fn an_undo_another_session_undid_stops_redoing() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        let undo = history.undo_last(&mut sm).unwrap();
        sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo)))
            .unwrap();
        let log_len = sm.log().len();

        assert!(matches!(
            history.redo_last(&mut sm),
            Err(RedoLastError::RedoneElsewhere)
        ));
        assert_eq!(sm.log().len(), log_len);
        assert_eq!(sm.current(), "a");
    }

    /// An action the session undid itself, by an undo targeting it, lies below that undo, so it is
    /// in effect again by the time it is the latest action.
    #[test]
    fn an_action_the_session_undid_itself_is_undone_after_that_undo() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        let a = push_in(&mut history, &mut sm, 'a');
        history
            .apply(
                &mut sm,
                LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a)),
            )
            .unwrap();
        assert_eq!(sm.current(), "");

        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "a");
        history.undo_last(&mut sm).unwrap();

        assert_eq!(sm.current(), "");
        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::NothingToUndo)
        ));
    }

    #[test]
    fn an_undo_an_entry_of_another_session_depends_on_is_rejected() {
        let mut sm = StateMachine::<i64, I64Transition>::new();
        let mut history = SessionHistory::new();
        history.apply(&mut sm, apply_entry(5)).unwrap();
        sm.apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Apply(Box::new(RequireAtLeast(5))),
        ))
        .unwrap();
        let log_len = sm.log().len();

        let result = history.undo_last(&mut sm);

        assert!(matches!(result, Err(UndoLastError::Rejected(_))));
        assert_eq!(sm.log().len(), log_len);
        assert_eq!(*sm.current(), 5);
        // the action is still the one to undo
        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::Rejected(_))
        ));
    }
}

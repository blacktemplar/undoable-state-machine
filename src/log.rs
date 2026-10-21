//! The entries of a [`StateMachine`]'s append-only log and the ids that identify them.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(doc)]
use crate::StateMachine;

/// Identifies a single [`LogEntry`] so a later entry can target it via [`LogEntryKind::Undo`].
///
/// Caller-assigned via [`TransitionId::new`]. Must be unique across a single [`StateMachine`]'s
/// log.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TransitionId(Uuid);

impl Default for TransitionId {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionId {
    /// Generates a new, randomly-assigned id.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::TransitionId;
    ///
    /// let id = TransitionId::new();
    /// assert_ne!(id, TransitionId::new());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// One entry in a [`StateMachine`]'s append-only log.
///
/// With the `serde` feature, an entry is (de)serializable whenever the transition type `T` is, so
/// a whole log (`Vec<LogEntry<T>>`), including its undo entries, can be persisted. Note that
/// `Box<dyn Transition<S>>` is not (de)serializable - use an enum of the transitions, or a crate
/// like [`typetag`](https://docs.rs/typetag), as `T` if you need to persist a log.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LogEntry<T> {
    id: TransitionId,
    kind: LogEntryKind<T>,
}

impl<T> LogEntry<T> {
    /// Creates a new log entry with the given id and kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{LogEntry, LogEntryKind, TransitionId};
    ///
    /// let entry = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(5));
    /// match entry.kind() {
    ///     LogEntryKind::Apply(v) => assert_eq!(*v, 5),
    ///     LogEntryKind::Undo(_) | LogEntryKind::Replace { .. } => unreachable!(),
    /// }
    /// ```
    #[must_use]
    pub fn new(id: TransitionId, kind: LogEntryKind<T>) -> Self {
        Self { id, kind }
    }

    /// Uniquely identifies this entry so a later entry can target it via [`LogEntryKind::Undo`] or
    /// [`LogEntryKind::Replace`].
    pub fn id(&self) -> TransitionId {
        self.id
    }

    /// What this entry does: apply a transition, undo a previous entry, or replace a previous
    /// transition.
    pub fn kind(&self) -> &LogEntryKind<T> {
        &self.kind
    }
}

/// Represents applying a transition, undoing a previous entry, or replacing a previous transition.
///
/// Undo is just another log entry, which is what makes undo itself undoable. Undoing an `Undo`
/// reactivates its target, which is basically a redo.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum LogEntryKind<T> {
    /// Applies the given transition.
    Apply(T),
    /// Undoes the entry with the given id
    Undo(TransitionId),
    /// Runs `with` in place of the [`LogEntryKind::Apply`] with the id `target`, at that entry's
    /// position in the log rather than at the end.
    ///
    /// `target` is always the original `Apply`, never another `Replace`: replacing a transition
    /// again is another `Replace` of the same `Apply`. The latest `Replace` of an `Apply` that is
    /// in effect runs in its place, so undoing that `Replace` falls back to the previous one, or
    /// to the original transition if there is none. Undoing the targeted `Apply` itself undoes it
    /// together with all of its replacements.
    Replace {
        /// The id of the `Apply` entry whose transition is replaced.
        target: TransitionId,
        /// The transition that runs in place of the target's.
        with: T,
    },
}

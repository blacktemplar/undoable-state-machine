#![doc = include_str!("../README.md")]

use std::{collections::HashSet, error::Error, fmt};

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

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

/// Typically a log will contain many different transitions (using `Box<dyn Transition<S> + ...>`)
/// which can also return many different types of errors.
pub type TransitionError = Box<dyn Error + Send + Sync>;

/// A transition that mutates state, split into a pure pre-check and the mutation itself.
///
/// # Examples
///
/// ```
/// use undoable_state_machine::{Transition, TransitionError};
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
/// ```
pub trait Transition<S> {
    /// Checks whether the transition is applicable to `state`, without mutating it.
    ///
    /// # Errors
    /// Returns an error if the transition is not applicable to the state.
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError>;

    /// Applies the transition by mutating `state` in place.
    ///
    /// Only called after [`Transition::check_applicable`] returned `Ok(())`, and must be
    /// deterministic: the same input state must always produce the same mutation, since
    /// [`StateMachine::apply`] relies on this to recover by replaying the log if this method
    /// fails. On error, treat `state` as dirty and don't rely on its contents. Errors here
    /// should be rare - catch problems early in [`Transition::check_applicable`] instead.
    ///
    /// # Errors
    /// An error occurs during mutation that was not caught in [`Transition::check_applicable`].
    fn apply(&self, state: &mut S) -> Result<(), TransitionError>;
}

/// Useful when using `Box<dyn Transition<...> + ...>` as `T` in [`StateMachine`]
impl<S, T: ?Sized + Transition<S>> Transition<S> for Box<T> {
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError> {
        (**self).check_applicable(state)
    }
    fn apply(&self, state: &mut S) -> Result<(), TransitionError> {
        (**self).apply(state)
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
    ///     LogEntryKind::Undo(_) => unreachable!(),
    /// }
    /// ```
    #[must_use]
    pub fn new(id: TransitionId, kind: LogEntryKind<T>) -> Self {
        Self { id, kind }
    }

    /// Uniquely identifies this entry so a later entry can target it via [`LogEntryKind::Undo`].
    pub fn id(&self) -> TransitionId {
        self.id
    }

    /// What this entry does: apply a transition, or undo a previous entry.
    pub fn kind(&self) -> &LogEntryKind<T> {
        &self.kind
    }
}

/// Represents applying a transition or undoing a previous transition.
///
/// Undo is just another log entry, which is what makes undo itself undoable. Undoing an `Undo`
/// reactivates its target, which is basically a redo.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum LogEntryKind<T> {
    /// Applies the given transition.
    Apply(T),
    /// Undoes the transition with the given id
    Undo(TransitionId),
}

/// An error from [`StateMachine::apply`] or [`StateMachine::build`].
#[derive(Debug)]
pub enum StateMachineError {
    /// A transition returned an error
    Transition(TransitionId, TransitionError),
    /// An `Undo` targeted an id not present anywhere earlier in the log.
    UnknownTarget(TransitionId),
    /// An entry's id already appears earlier in the log. [`TransitionId`] must be unique, reusing
    /// one would make `Undo` targets ambiguous.
    DuplicateId(TransitionId),
}

impl fmt::Display for StateMachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateMachineError::Transition(id, err) => {
                write!(f, "transition {id:?} failed: {err}")
            }
            StateMachineError::UnknownTarget(id) => {
                write!(f, "undo targets unknown transition {id:?}")
            }
            StateMachineError::DuplicateId(id) => {
                write!(f, "transition id {id:?} already appears in the log")
            }
        }
    }
}

impl Error for StateMachineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StateMachineError::Transition(_, err) => Some(err.as_ref()),
            StateMachineError::UnknownTarget(_) | StateMachineError::DuplicateId(_) => None,
        }
    }
}

pub struct StateMachine<S, T> {
    log: Vec<LogEntry<T>>,
    /// Every id currently in `log`, kept in sync with it so id lookups (duplicate rejection,
    /// `Undo` target existence) are O(1) instead of a linear scan over a potentially long log.
    known_ids: HashSet<TransitionId>,
    current: S,
}

impl<S: fmt::Debug, T: fmt::Debug> fmt::Debug for StateMachine<S, T> {
    // Omits the internal `known_ids` id cache, which is redundant with `log`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateMachine")
            .field("log", &self.log)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl<S: Default, T> Default for StateMachine<S, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, T> StateMachine<S, T> {
    /// Returns the current state: the result of replaying every log entry that is still active
    /// (i.e. not cancelled by a later, still-active [`LogEntryKind::Undo`]).
    pub fn current(&self) -> &S {
        &self.current
    }

    /// Returns the full append-only log, including entries that have since been undone.
    pub fn log(&self) -> &[LogEntry<T>] {
        &self.log
    }

    /// Returns the log entries that are in effect, i.e. not cancelled by a later
    /// [`LogEntryKind::Undo`] that is in effect itself, from the newest to the oldest.
    ///
    /// The iterator is lazy: it only walks the log as far back as it is advanced, so finding the
    /// latest entry in effect that matches some condition costs only the part of the log that
    /// lies after it.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
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
    /// let (add_5, add_7, undo_add_7) = (
    ///     TransitionId::new(),
    ///     TransitionId::new(),
    ///     TransitionId::new(),
    /// );
    /// let log: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    ///     LogEntry::new(add_5, LogEntryKind::Apply(Box::new(Add(5)))),
    ///     LogEntry::new(add_7, LogEntryKind::Apply(Box::new(Add(7)))),
    ///     LogEntry::new(undo_add_7, LogEntryKind::Undo(add_7)),
    /// ];
    /// let sm = StateMachine::build(log)?;
    /// let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
    /// assert_eq!(in_effect, [undo_add_7, add_5]);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    pub fn rev_in_effect(&self) -> impl Iterator<Item = &LogEntry<T>> {
        rev_log_with_status(&self.log).filter_map(|(entry, in_effect)| in_effect.then_some(entry))
    }
}

/// Every entry of `log` from the newest to the oldest, each with whether it is in effect, i.e. not
/// cancelled by a later [`LogEntryKind::Undo`] that is in effect itself.
fn rev_log_with_status<T>(log: &[LogEntry<T>]) -> impl Iterator<Item = (&LogEntry<T>, bool)> {
    // only a later entry can cancel an entry, so by the time the backward walk reaches an entry it
    // has seen every undo in effect that could cancel it
    let mut cancelled = HashSet::new();
    log.iter().rev().map(move |entry| {
        // ids are unique, so a cancelled id is never looked up again
        let in_effect = !cancelled.remove(&entry.id);
        if in_effect {
            if let LogEntryKind::Undo(target) = entry.kind {
                cancelled.insert(target);
            }
        }
        (entry, in_effect)
    })
}

impl<S: Default, T> StateMachine<S, T> {
    /// Creates an empty state machine with `S::default()` as the current state and no log
    /// entries.
    ///
    /// Use [`Self::build`] to reconstruct a state machine from an existing log instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{StateMachine, Transition};
    ///
    /// let sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();
    /// assert_eq!(*sm.current(), 0);
    /// assert!(sm.log().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            log: Vec::default(),
            known_ids: HashSet::default(),
            current: Default::default(),
        }
    }
}

impl<S: Default, T: Transition<S>> StateMachine<S, T> {
    /// Reconstructs a state machine from a previously recorded log, e.g. one persisted elsewhere
    /// or merged from multiple sources.
    ///
    /// Equivalent to starting from [`Self::new`] and calling [`Self::apply`] with each entry in
    /// order, but computes `current` in a single backward-then-forward pass instead of replaying
    /// the growing log again after every entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
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
    /// let add_7_transition_id = TransitionId::new();
    /// let log: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(5)))),
    ///     LogEntry::new(add_7_transition_id, LogEntryKind::Apply(Box::new(Add(7)))),
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(-3)))),
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Undo(add_7_transition_id)),
    /// ];
    /// let sm = StateMachine::build(log)?;
    /// assert_eq!(*sm.current(), 2);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::apply`]'s `Undo` case: an unknown
    /// undo target, or an active transition that fails to apply. Also returns
    /// [`StateMachineError::DuplicateId`] if two entries in `log` share the same id.
    pub fn build(log: Vec<LogEntry<T>>) -> Result<Self, StateMachineError> {
        let mut known_ids = HashSet::with_capacity(log.len());
        for entry in &log {
            if !known_ids.insert(entry.id) {
                return Err(StateMachineError::DuplicateId(entry.id));
            }
        }
        let current = Self::replay(&log)?;
        Ok(Self {
            log,
            known_ids,
            current,
        })
    }

    /// Applies `entry` and appends it to the log. On error, the log and [`Self::current`] are
    /// left unchanged.
    ///
    /// If the logs contain undos then calling this method is more efficient than applying all
    /// entries one by one with [`Self::apply`] since undone entries will never get applied in the
    /// first place. Note, however that this changes the semantics in case an undone entry would
    /// error. If you call [`Self::apply`] for each entry then it would error before applying the
    /// undo, while [`Self::apply`] fully ignores the undone entry and never applies it and never
    /// surfaces the error.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
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
    /// let mut sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Apply(Box::new(Add(5))),
    /// ))?;
    /// assert_eq!(*sm.current(), 5);
    /// let add_7_transition_id = TransitionId::new();
    /// sm.apply(LogEntry::new(
    ///     add_7_transition_id,
    ///     LogEntryKind::Apply(Box::new(Add(7))),
    /// ))?;
    /// assert_eq!(*sm.current(), 12);
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Apply(Box::new(Add(-3))),
    /// ))?;
    /// assert_eq!(*sm.current(), 9);
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Undo(add_7_transition_id),
    /// ))?;
    /// assert_eq!(*sm.current(), 2);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::Transition`] if the transition (for `Apply`) or some later
    /// active entry that depended on the target (for `Undo`) fails;
    /// [`StateMachineError::UnknownTarget`] if `Undo` targets an id not present earlier in the
    /// log. Returns [`StateMachineError::DuplicateId`] if `entry`'s id already appears in the
    /// log.
    ///
    /// # Panics
    ///
    /// Panics if healing the state after a failed `apply` (see [`Transition::apply`]'s
    /// determinism contract) itself fails to replay. This indicates a `Transition` impl that
    /// violates that contract, not a reachable failure under a correct one.
    pub fn apply(&mut self, entry: LogEntry<T>) -> Result<(), StateMachineError> {
        if self.known_ids.contains(&entry.id) {
            return Err(StateMachineError::DuplicateId(entry.id));
        }
        match &entry.kind {
            LogEntryKind::Apply(transition) => match transition.check_applicable(&self.current) {
                Ok(()) => match transition.apply(&mut self.current) {
                    Ok(()) => {
                        self.known_ids.insert(entry.id);
                        self.log.push(entry);
                        Ok(())
                    }
                    Err(err) => {
                        // we need to replay the state
                        // `active` is exactly what it was before this call, and that already
                        // replayed cleanly to produce the old `current`, so re-replaying it here
                        // cannot fail.
                        self.current = Self::replay(&self.log)
                            .expect("previously-active entries must still replay");
                        Err(StateMachineError::Transition(entry.id, err))
                    }
                },
                Err(err) => Err(StateMachineError::Transition(entry.id, err)),
            },
            LogEntryKind::Undo(target) => {
                let id = entry.id;
                if !self.known_ids.insert(id) {
                    return Err(StateMachineError::UnknownTarget(*target));
                }
                self.log.push(entry);
                match Self::replay(&self.log) {
                    Ok(state) => {
                        self.current = state;
                        Ok(())
                    }
                    Err(err) => {
                        self.log.pop();
                        self.known_ids.remove(&id);
                        Err(err)
                    }
                }
            }
        }
    }

    /// Replays every active log entry, in log order, onto a fresh `S::default()`.
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::UnknownTarget`] if an `Undo` targets an id that never appears
    /// among the entries scanned, or [`StateMachineError::Transition`] if any active transition's
    /// [`Transition::check_applicable`] or [`Transition::apply`] fails.
    fn replay(entries: &[LogEntry<T>]) -> Result<S, StateMachineError> {
        // the targets of the undos in effect that the backward walk hasn't reached yet; an entry
        // that is not in effect is always such a target
        let mut unresolved = HashSet::new();
        let mut active = Vec::with_capacity(entries.len());
        for (entry, in_effect) in rev_log_with_status(entries) {
            if !in_effect {
                unresolved.remove(&entry.id);
                continue;
            }
            match &entry.kind {
                LogEntryKind::Apply(_) => active.push(entry),
                LogEntryKind::Undo(target) => {
                    unresolved.insert(*target);
                }
            }
        }

        if let Some(id) = unresolved.into_iter().next() {
            return Err(StateMachineError::UnknownTarget(id));
        }

        // The scan above runs backwards (an `Undo` can only cancel an entry before it), so
        // `active` came out newest-first. Transitions don't commute in general, they have to be
        // applied in log order.
        let active = active.into_iter().rev();

        let mut state = S::default();
        for entry in active {
            let LogEntryKind::Apply(transition) = &entry.kind else {
                unreachable!("We only added apply kinds to active")
            };
            transition
                .check_applicable(&state)
                .map_err(|err| StateMachineError::Transition(entry.id, err))?;
            transition
                .apply(&mut state)
                .map_err(|err| StateMachineError::Transition(entry.id, err))?;
        }

        Ok(state)
    }
}

/// Serializes the state machine as its log alone, the full append-only log including entries that
/// have since been undone. [`StateMachine::current`] is derived state and is recomputed when
/// deserializing, so it is not part of the serialized form.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use undoable_state_machine::{
///     LogEntry, LogEntryKind, StateMachine, Transition, TransitionError, TransitionId,
/// };
///
/// // `T` has to be (de)serializable itself, so enumerate the transitions instead of using
/// // `Box<dyn Transition<i64>>`.
/// #[derive(Serialize, Deserialize)]
/// enum Arith {
///     Add(i64),
///     Mul(i64),
/// }
///
/// impl Transition<i64> for Arith {
///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
///         Ok(())
///     }
///
///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
///         match self {
///             Arith::Add(v) => *state += v,
///             Arith::Mul(v) => *state *= v,
///         }
///         Ok(())
///     }
/// }
///
/// let mut sm = StateMachine::<i64, Arith>::new();
/// sm.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Apply(Arith::Add(5)),
/// ))?;
/// let doubling_id = TransitionId::new();
/// sm.apply(LogEntry::new(
///     doubling_id,
///     LogEntryKind::Apply(Arith::Mul(2)),
/// ))?;
/// let undo_id = TransitionId::new();
/// sm.apply(LogEntry::new(undo_id, LogEntryKind::Undo(doubling_id)))?;
/// assert_eq!(*sm.current(), 5);
///
/// let json = serde_json::to_string(&sm)?;
/// let mut restored: StateMachine<i64, Arith> = serde_json::from_str(&json)?;
/// assert_eq!(*restored.current(), 5);
///
/// // The undo entry round trips too, so it can still be undone itself.
/// restored.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Undo(undo_id),
/// ))?;
/// assert_eq!(*restored.current(), 10);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[cfg(feature = "serde")]
impl<S, T: Serialize> Serialize for StateMachine<S, T> {
    fn serialize<Ser: Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        self.log.serialize(serializer)
    }
}

/// Deserializes a log and reconstructs the state machine from it with [`StateMachine::build`],
/// which recomputes [`StateMachine::current`].
///
/// Deserializing therefore fails - with the [`StateMachineError`] as the message - on a log that
/// `build` would reject: a duplicate entry id, an undo of an unknown target, or an active
/// transition that doesn't replay cleanly.
#[cfg(feature = "serde")]
impl<'de, S: Default, T: Transition<S> + Deserialize<'de>> Deserialize<'de> for StateMachine<S, T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let log = Vec::<LogEntry<T>>::deserialize(deserializer)?;
        Self::build(log).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;

    use super::*;

    struct Add(i64);

    pub type I64Transition = Box<dyn Transition<i64>>;

    impl Transition<i64> for Add {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Ok(())
        }
    }

    // Always reports itself as applicable, then fails inside `apply` after
    // already mutating `state`. Models a transition whose `apply` breaks the
    // "should be rare/avoided" guidance, to exercise that failure path.
    struct FailDuringApply(i64);

    impl Transition<i64> for FailDuringApply {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Err("boom".to_string().into())
        }
    }

    // Not applicable unless `state` is already at least `self.0`. Used to make a
    // later entry's success depend on an earlier one still being active.
    struct RequireAtLeast(i64);

    impl Transition<i64> for RequireAtLeast {
        fn check_applicable(&self, state: &i64) -> Result<(), TransitionError> {
            if *state < self.0 {
                Err(format!("need at least {}, have {}", self.0, state).into())
            } else {
                Ok(())
            }
        }

        fn apply(&self, _state: &mut i64) -> Result<(), TransitionError> {
            Ok(())
        }
    }

    // Passes `check_applicable` unconditionally, but `apply` fails unless
    // `state` is already at least `self.0`. Unlike `RequireAtLeast`, this
    // makes the *dependency* break surface from `apply` instead of
    // `check_applicable`.
    struct FailApplyIfBelow(i64);

    impl Transition<i64> for FailApplyIfBelow {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            if *state < self.0 {
                Err(format!("need at least {} to apply, have {}", self.0, state).into())
            } else {
                *state += 1;
                Ok(())
            }
        }
    }

    fn apply_entry(value: i64) -> LogEntry<I64Transition> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(value))),
        }
    }

    // Appends a character to the state. Unlike `Add`, pushes don't commute, so
    // replaying the active set in the wrong order shows up as a different
    // string rather than the same number.
    struct Push(char);

    pub type StringTransition = Box<dyn Transition<String>>;

    impl Transition<String> for Push {
        fn check_applicable(&self, _state: &String) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut String) -> Result<(), TransitionError> {
            state.push(self.0);
            Ok(())
        }
    }

    fn push_entry(c: char) -> LogEntry<StringTransition> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Push(c))),
        }
    }

    #[test]
    fn build_applies_active_transitions_in_log_order() {
        let sm = StateMachine::<String, StringTransition>::build(vec![
            push_entry('a'),
            push_entry('b'),
            push_entry('c'),
        ])
        .unwrap();

        assert_eq!(sm.current(), "abc");
    }

    #[test]
    fn undo_keeps_the_surviving_transitions_in_log_order() {
        let mut sm = StateMachine::<String, StringTransition>::default();
        sm.apply(push_entry('a')).unwrap();
        let x = push_entry('x');
        let x_id = x.id;
        sm.apply(x).unwrap();
        sm.apply(push_entry('b')).unwrap();
        assert_eq!(sm.current(), "axb");

        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(x_id),
        })
        .unwrap();

        assert_eq!(sm.current(), "ab");
    }

    #[test]
    fn build_reactivates_an_undone_entry_in_its_original_slot() {
        let a = push_entry('a');
        let x = push_entry('x');
        let x_id = x.id;
        let b = push_entry('b');
        let undo_x = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(x_id),
        };
        let undo_x_id = undo_x.id;
        let undo_undo_x = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_x_id),
        };

        let undone = StateMachine::build(vec![a, x, b, undo_x]).unwrap();
        assert_eq!(undone.current(), "ab");

        let a = push_entry('a');
        let x = LogEntry {
            id: x_id,
            kind: LogEntryKind::Apply(Box::new(Push('x')) as StringTransition),
        };
        let b = push_entry('b');
        let undo_x = LogEntry {
            id: undo_x_id,
            kind: LogEntryKind::Undo(x_id),
        };
        let redone = StateMachine::build(vec![a, x, b, undo_x, undo_undo_x]).unwrap();

        // `x` is active again, back in its original position - not appended at the end.
        assert_eq!(redone.current(), "axb");
    }

    #[test]
    fn append_success_updates_current() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        sm.apply(apply_entry(5)).unwrap();
        sm.apply(apply_entry(3)).unwrap();
        assert_eq!(*sm.current(), 8);
    }

    #[test]
    fn append_rejects_a_not_applicable_transition_without_mutating_state() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        sm.apply(apply_entry(5)).unwrap();

        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        };
        let failing_id = entry.id;
        let err = sm.apply(entry).unwrap_err();

        match err {
            StateMachineError::Transition(id, _) => assert_eq!(id, failing_id),
            StateMachineError::UnknownTarget(_) | StateMachineError::DuplicateId(_) => {
                panic!("expected StateMachineError::Transition")
            }
        }
        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 1);
    }

    #[test]
    fn append_apply_failure_does_not_grow_log() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        sm.apply(apply_entry(5)).unwrap();

        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailDuringApply(100))),
        };
        let failing_id = entry.id;
        let err = sm.apply(entry).unwrap_err();

        match err {
            StateMachineError::Transition(id, err) => {
                assert_eq!(id, failing_id);
                assert_eq!(err.to_string(), "boom");
            }
            StateMachineError::UnknownTarget(_) | StateMachineError::DuplicateId(_) => {
                panic!("expected StateMachineError::Transition")
            }
        }
        // The failing entry is never recorded in the log. `current` was
        // already mutated by `apply` before it failed though, so per
        // `Transition::apply`'s contract it must now be treated as dirty and
        // not relied upon. Recovery means rebuilding from the (unaffected) log.
        assert_eq!(sm.log().len(), 1);

        let rebuilt = StateMachine::<i64, I64Transition>::build(vec![apply_entry(5)]).unwrap();
        assert_eq!(*rebuilt.current(), 5);
    }

    #[test]
    fn undo_cancels_a_prior_apply() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        let first: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(5))),
        };
        let first_id = first.id;
        sm.apply(first).unwrap();
        sm.apply(apply_entry(3)).unwrap();
        assert_eq!(*sm.current(), 8);

        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(first_id),
        })
        .unwrap();

        assert_eq!(*sm.current(), 3);
    }

    #[test]
    fn undo_of_unknown_target_is_rejected() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        sm.apply(apply_entry(5)).unwrap();

        let err = sm
            .apply(LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Undo(TransitionId::new()),
            })
            .unwrap_err();

        assert!(matches!(err, StateMachineError::UnknownTarget(_)));
        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 1);
    }

    #[test]
    fn undo_that_breaks_a_later_entry_is_rejected_and_blames_that_entry() {
        let mut sm = StateMachine::<i64, I64Transition>::default();

        let add: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(10))),
        };
        let add_id = add.id;
        sm.apply(add).unwrap();

        // Only succeeds while `add`'s effect is still active.
        let guard: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        };
        let guard_id = guard.id;
        sm.apply(guard).unwrap();

        let err = sm
            .apply(LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Undo(add_id),
            })
            .unwrap_err();

        match err {
            StateMachineError::Transition(id, _) => assert_eq!(id, guard_id),
            StateMachineError::UnknownTarget(_) | StateMachineError::DuplicateId(_) => {
                panic!("expected StateMachineError::Transition")
            }
        }
        // The undo itself is rejected: current and the log are unaffected.
        assert_eq!(*sm.current(), 10);
        assert_eq!(sm.log().len(), 2);
    }

    #[test]
    fn undo_that_breaks_a_later_entrys_apply_is_rejected_and_blames_that_entry() {
        let mut sm = StateMachine::<i64, I64Transition>::default();

        let add: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(10))),
        };
        let add_id = add.id;
        sm.apply(add).unwrap();

        // Succeeds now, while `add`'s effect keeps `state` at least 10, but
        // its `check_applicable` doesn't encode that dependency - only its
        // `apply` does.
        let guard: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailApplyIfBelow(10))),
        };
        let guard_id = guard.id;
        sm.apply(guard).unwrap();
        assert_eq!(*sm.current(), 11);

        let err = sm
            .apply(LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Undo(add_id),
            })
            .unwrap_err();

        match err {
            StateMachineError::Transition(id, _) => assert_eq!(id, guard_id),
            StateMachineError::UnknownTarget(_) | StateMachineError::DuplicateId(_) => {
                panic!("expected StateMachineError::Transition")
            }
        }
        // The undo itself is rejected: current and the log are unaffected.
        assert_eq!(*sm.current(), 11);
        assert_eq!(sm.log().len(), 2);
    }

    #[test]
    fn apply_rejects_a_duplicate_id() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        let entry = apply_entry(5);
        let id = entry.id;
        sm.apply(entry).unwrap();

        let dup: LogEntry<I64Transition> = LogEntry {
            id,
            kind: LogEntryKind::Apply(Box::new(Add(3))),
        };
        let err = sm.apply(dup).unwrap_err();

        match err {
            StateMachineError::DuplicateId(dup_id) => assert_eq!(dup_id, id),
            StateMachineError::UnknownTarget(_) | StateMachineError::Transition(_, _) => {
                panic!("expected StateMachineError::DuplicateId")
            }
        }
        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 1);
    }

    #[test]
    fn build_rejects_a_duplicate_id() {
        let a = apply_entry(5);
        let id = a.id;
        let b: LogEntry<I64Transition> = LogEntry {
            id,
            kind: LogEntryKind::Apply(Box::new(Add(3))),
        };

        match StateMachine::<i64, I64Transition>::build(vec![a, b]) {
            Err(StateMachineError::DuplicateId(dup_id)) => assert_eq!(dup_id, id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::DuplicateId, got {other:?}"),
        }
    }

    #[test]
    fn build_computes_current_from_a_log() {
        let a = apply_entry(5);
        let a_id = a.id;
        let b = apply_entry(3);
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };

        let sm = StateMachine::build(vec![a, b, undo_a]).unwrap();

        assert_eq!(*sm.current(), 3);
        assert_eq!(sm.log().len(), 3);
    }

    #[test]
    fn build_with_undone_undo_computes_current() {
        let a_id = TransitionId::new();
        let a = LogEntry {
            id: a_id,
            kind: LogEntryKind::Apply(Box::new(Add(5))),
        };
        let undo_a_id = TransitionId::new();
        let undo_a = LogEntry {
            id: undo_a_id,
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 3);
    }

    #[test]
    fn build_surfaces_the_failing_transition() {
        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailDuringApply(100))),
        };
        let failing_id = entry.id;

        match StateMachine::<i64, I64Transition>::build(vec![entry]) {
            Err(StateMachineError::Transition(id, err)) => {
                assert_eq!(id, failing_id);
                assert_eq!(err.to_string(), "boom");
            }
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::Transition, got {other:?}"),
        }
    }

    #[test]
    fn build_rejects_undo_of_an_unknown_target() {
        let unknown_id = TransitionId::new();
        let undo = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(unknown_id),
        };

        match StateMachine::<i64, I64Transition>::build(vec![undo]) {
            Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, unknown_id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn build_rejects_undo_appearing_before_its_target() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };

        match StateMachine::<i64, I64Transition>::build(vec![undo_a, a]) {
            Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, a_id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn build_allows_undoing_an_undo_to_reactivate_the_original() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_a_id = undo_a.id;
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        assert_eq!(*sm.current(), 5);
    }

    #[test]
    fn rev_in_effect_skips_an_undone_undo_but_not_its_target() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_a_id = undo_a.id;
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };
        let undo_undo_a_id = undo_undo_a.id;

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
        assert_eq!(in_effect, [undo_undo_a_id, a_id]);
    }

    /// What a random `apply` call in the property test below does: either
    /// append a new `Add`, or undo a previously-appended entry (picked by
    /// index into `all_ids`, wrapped so every index is valid).
    #[derive(Clone, Debug)]
    enum RandomOp {
        Apply(i64),
        Undo(usize),
    }

    enum MirrorKind {
        Apply(i64),
        Undo(TransitionId),
    }

    proptest::proptest! {
        // The crate's core invariant: `current()` is always exactly what
        // you'd get by replaying the log's active set from scratch. We
        // check it after every step of a random sequence of applies/undos,
        // rebuilding independently via `StateMachine::build` from a mirror
        // of the entries the state machine actually accepted.
        #[test]
        fn current_matches_a_from_scratch_replay_of_the_log(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    (-1_000_000i64..1_000_000i64).prop_map(RandomOp::Apply),
                    proptest::prelude::any::<usize>().prop_map(RandomOp::Undo),
                ],
                0..50,
            )
        ) {
            let mut sm = StateMachine::<i64, I64Transition>::default();
            let mut mirror: Vec<(TransitionId, MirrorKind)> = Vec::new();
            let mut all_ids: Vec<TransitionId> = Vec::new();

            for op in ops {
                let id = TransitionId::new();
                let (kind, mirror_kind) = match op {
                    RandomOp::Apply(v) => (
                        LogEntryKind::Apply(Box::new(Add(v)) as I64Transition),
                        MirrorKind::Apply(v),
                    ),
                    RandomOp::Undo(idx) => {
                        if all_ids.is_empty() {
                            continue;
                        }
                        let target = all_ids[idx % all_ids.len()];
                        (LogEntryKind::Undo(target), MirrorKind::Undo(target))
                    }
                };

                if sm.apply(LogEntry { id, kind }).is_ok() {
                    mirror.push((id, mirror_kind));
                    all_ids.push(id);
                }

                let rebuilt_log: Vec<LogEntry<I64Transition>> = mirror
                    .iter()
                    .map(|(id, kind)| {
                        let kind = match kind {
                            MirrorKind::Apply(v) => {
                                LogEntryKind::Apply(Box::new(Add(*v)) as I64Transition)
                            }
                            MirrorKind::Undo(target) => LogEntryKind::Undo(*target),
                        };
                        LogEntry { id: *id, kind }
                    })
                    .collect();
                let rebuilt = StateMachine::<i64, I64Transition>::build(rebuilt_log).unwrap();

                proptest::prop_assert_eq!(*sm.current(), *rebuilt.current());
            }
        }

        // The same invariant, but checked against a model that shares no code
        // with `replay` and over a transition that doesn't commute. Comparing
        // `apply` against `build` (above) cannot see a replay that runs the
        // active set in the wrong order, since both go through `replay`.
        #[test]
        fn current_matches_an_independent_model_of_the_active_set(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::char::range('a', 'z').prop_map(PushOp::Push),
                    proptest::prelude::any::<usize>().prop_map(PushOp::Undo),
                ],
                0..50,
            )
        ) {
            let mut sm = StateMachine::<String, StringTransition>::default();
            let mut mirror: Vec<(TransitionId, PushKind)> = Vec::new();

            for op in ops {
                let id = TransitionId::new();
                let kind = match op {
                    PushOp::Push(c) => PushKind::Push(c),
                    PushOp::Undo(idx) => {
                        if mirror.is_empty() {
                            continue;
                        }
                        PushKind::Undo(mirror[idx % mirror.len()].0)
                    }
                };
                let entry = LogEntry {
                    id,
                    kind: match &kind {
                        PushKind::Push(c) => {
                            LogEntryKind::Apply(Box::new(Push(*c)) as StringTransition)
                        }
                        PushKind::Undo(target) => LogEntryKind::Undo(*target),
                    },
                };

                // `Push` is applicable to any state and never fails, so no
                // entry here can be rejected - every op lands in the log.
                sm.apply(entry).unwrap();
                mirror.push((id, kind));

                proptest::prop_assert_eq!(sm.current(), &expected_state(&mirror));
                let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
                let expected: Vec<_> = mirror
                    .iter()
                    .zip(expected_active(&mirror))
                    .rev()
                    .filter_map(|((id, _), active)| active.then_some(*id))
                    .collect();
                proptest::prop_assert_eq!(in_effect, expected);
            }
        }
    }

    /// What a random `apply` call in the property test above does, over
    /// non-commuting `Push` transitions: either append a character, or undo a
    /// previously-appended entry (picked by index into `mirror`, wrapped so
    /// every index is valid).
    #[derive(Clone, Debug)]
    enum PushOp {
        Push(char),
        Undo(usize),
    }

    enum PushKind {
        Push(char),
        Undo(TransitionId),
    }

    /// Model of what the state machine's `current()` should be for a log of
    /// `Push`/`Undo` entries, written independently of `StateMachine::replay`:
    /// an entry is active unless some still-active `Undo` targets it, and the
    /// surviving pushes apply in log order.
    fn expected_state(entries: &[(TransitionId, PushKind)]) -> String {
        entries
            .iter()
            .zip(expected_active(entries))
            .filter_map(|((_, kind), active)| match kind {
                PushKind::Push(c) if active => Some(*c),
                PushKind::Push(_) | PushKind::Undo(_) => None,
            })
            .collect()
    }

    /// Whether each of `entries` is active, i.e. not targeted by a still-active `Undo`, written
    /// independently of `rev_log_with_status`.
    fn expected_active(entries: &[(TransitionId, PushKind)]) -> Vec<bool> {
        let mut active = vec![false; entries.len()];
        // Back to front: an `Undo` can only target an earlier entry, so
        // whether entry `index` survives depends only on already-decided ones.
        for index in (0..entries.len()).rev() {
            let target_id = entries[index].0;
            active[index] = !entries
                .iter()
                .enumerate()
                .skip(index + 1)
                .any(|(i, (_, kind))| {
                    active[i] && matches!(kind, PushKind::Undo(target) if *target == target_id)
                });
        }
        active
    }

    #[cfg(feature = "serde")]
    mod serde_support {
        use super::*;

        // A transition type that is itself (de)serializable, which `Box<dyn
        // Transition<i64>>` can't be. `Mul` makes the log non-commutative, so
        // a round trip that lost the log's order would be visible.
        #[derive(Debug, Serialize, Deserialize)]
        enum Arith {
            Add(i64),
            Mul(i64),
        }

        impl Transition<i64> for Arith {
            fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
                Ok(())
            }

            fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
                match self {
                    Arith::Add(v) => *state += v,
                    Arith::Mul(v) => *state *= v,
                }
                Ok(())
            }
        }

        #[test]
        fn a_log_round_trips_through_json() {
            let add = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Arith::Add(5)));
            let mul = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Arith::Mul(3)));
            let log = vec![add, mul];

            let json = serde_json::to_string(&log).unwrap();
            let restored: Vec<LogEntry<Arith>> = serde_json::from_str(&json).unwrap();

            assert_eq!(restored.len(), 2);
            assert_eq!(restored[0].id(), log[0].id());
            assert_eq!(restored[1].id(), log[1].id());
            // (5 + 0) * 3, not 0 * 3 + 5: the entries keep their order.
            assert_eq!(*StateMachine::build(restored).unwrap().current(), 15);
        }

        #[test]
        fn a_state_machine_round_trips_through_json_with_its_undos() {
            let mut sm = StateMachine::<i64, Arith>::new();
            sm.apply(LogEntry::new(
                TransitionId::new(),
                LogEntryKind::Apply(Arith::Add(5)),
            ))
            .unwrap();
            let doubling_id = TransitionId::new();
            sm.apply(LogEntry::new(
                doubling_id,
                LogEntryKind::Apply(Arith::Mul(2)),
            ))
            .unwrap();
            let undo_id = TransitionId::new();
            sm.apply(LogEntry::new(undo_id, LogEntryKind::Undo(doubling_id)))
                .unwrap();
            assert_eq!(*sm.current(), 5);

            let json = serde_json::to_string(&sm).unwrap();
            let mut restored: StateMachine<i64, Arith> = serde_json::from_str(&json).unwrap();

            // `current` is recomputed from the restored log, undone entry included.
            assert_eq!(*restored.current(), 5);
            assert_eq!(restored.log().len(), 3);

            // The undo entry survived as an entry, so it is still undoable.
            restored
                .apply(LogEntry::new(
                    TransitionId::new(),
                    LogEntryKind::Undo(undo_id),
                ))
                .unwrap();
            assert_eq!(*restored.current(), 10);
        }

        #[test]
        fn deserializing_a_state_machine_rejects_a_log_that_build_rejects() {
            let unknown_id = TransitionId::new();
            let log: Vec<LogEntry<Arith>> = vec![LogEntry::new(
                TransitionId::new(),
                LogEntryKind::Undo(unknown_id),
            )];
            let json = serde_json::to_string(&log).unwrap();

            let err = serde_json::from_str::<StateMachine<i64, Arith>>(&json).unwrap_err();

            assert!(
                err.to_string().contains("undo targets unknown transition"),
                "unexpected error: {err}"
            );
        }
    }
}

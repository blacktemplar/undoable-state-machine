#![doc = include_str!("../README.md")]

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use uuid::Uuid;

/// Identifies a single [`LogEntry`] so a later entry can target it via [`LogEntryKind::Undo`].
///
/// Caller-assigned via [`TransitionId::new`]. Must be unique across a single [`StateMachine`]'s
/// log.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
#[derive(Debug)]
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
    /// let log: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(5)))),
    /// ];
    /// let sm = StateMachine::build(log)?;
    /// assert_eq!(*sm.current(), 5);
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
    /// sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(5)))))?;
    /// assert_eq!(*sm.current(), 5);
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

    /// Replays every active log entry onto a fresh `S::default()`.
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::UnknownTarget`] if an `Undo` targets an id that never appears
    /// among the entries scanned, or [`StateMachineError::Transition`] if any active transition's
    /// [`Transition::check_applicable`] or [`Transition::apply`] fails.
    fn replay(entries: &[LogEntry<T>]) -> Result<S, StateMachineError> {
        // the keys are the cancelled transition ids the values are if we found them later on
        let mut cancelled = HashMap::new();
        let mut active = Vec::with_capacity(entries.len());
        for entry in entries.iter().rev() {
            match cancelled.get_mut(&entry.id) {
                Some(v) => *v = true,
                None => match &entry.kind {
                    LogEntryKind::Apply(_) => active.push(entry),
                    LogEntryKind::Undo(transition_id) => {
                        cancelled.insert(*transition_id, false);
                    }
                },
            }
        }

        for (id, found) in cancelled {
            if !found {
                return Err(StateMachineError::UnknownTarget(id));
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::Strategy;

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
    }
}

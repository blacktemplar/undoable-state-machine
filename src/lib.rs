use std::collections::HashMap;
use std::error::Error;

use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TransitionId(Uuid); // caller-assigned, must be globally unique

impl Default for TransitionId {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Transitions are dynamically dispatched, so their errors are too — a log
/// can freely mix transitions with unrelated error types.
pub type TransitionError = Box<dyn Error + Send + Sync>;

/// A transition that modifies state and can potentially error.
pub trait Transition<S> {
    /// Checks if the transition is applicable to the given state. This is used as a pre-check to
    /// prevent errors during mutating the state (which would invalidate the state).
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError>;

    /// Applies the transition by mutating `state` in place.
    ///
    /// Must produce the same mutation for the same input state, every time.
    ///
    /// [`Transition::apply`] should only be called if [`Transition::check_applicable`] returned
    /// `Ok(())`. Therefore, implementers of `apply` can assume that [`Transition::check_applicable`]
    /// just returned `Ok(())`.
    ///
    /// If this method returns an error callers should treat the state as dirty and don't use it
    /// anymore as there is no guarantee to its content. If they want to replay the full log.
    ///
    /// Since replaying can be costly errors should be rare and should be avoided if possible by
    /// catching problems early in [`Transition::check_applicable`].
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

pub struct LogEntry<T> {
    pub id: TransitionId,
    pub kind: LogEntryKind<T>,
}

pub enum LogEntryKind<T> {
    Apply(T),
    Undo(TransitionId), // targets an earlier entry's id
}

#[derive(Debug)]
pub enum StateMachineError {
    /// A transition returned an error and `current` is unaffected by this
    /// call. The id identifies whichever transition actually failed: for an
    /// `Apply` that's the entry itself; for an `Undo`, it can instead be a
    /// *different*, earlier entry that no longer applies cleanly once the
    /// undone entry is removed from the replay.
    Transition(TransitionId, TransitionError),
    /// An `Undo` targeted an id not present anywhere earlier in the log.
    /// Not a valid ordering — a caller merging logs from multiple sources
    /// must ensure every `Undo` comes after the entry it targets.
    UnknownTarget(TransitionId),
}

pub struct StateMachine<S, T> {
    log: Vec<LogEntry<T>>,
    current: S,
}

impl<S: Default, T> Default for StateMachine<S, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, T> StateMachine<S, T> {
    pub fn current(&self) -> &S {
        &self.current
    }

    pub fn log(&self) -> &[LogEntry<T>] {
        &self.log
    }

    fn contains_id(&self, id: TransitionId) -> bool {
        self.log.iter().any(|entry| entry.id == id)
    }
}

impl<S: Default, T> StateMachine<S, T> {
    pub fn new() -> Self {
        Self {
            log: Default::default(),
            current: Default::default(),
        }
    }
}
impl<S: Default, T: Transition<S>> StateMachine<S, T> {
    pub fn build(log: Vec<LogEntry<T>>) -> Result<Self, StateMachineError> {
        let current = Self::replay(&log)?;
        Ok(Self { log, current })
    }

    /// Applies the `entry` and appends it to the log. In case of an error the state will be
    /// unmodified (i.e. the [`Self::log`] as well as [`Self::current`] will be as before this
    /// call).
    pub fn apply(&mut self, entry: LogEntry<T>) -> Result<(), StateMachineError> {
        match &entry.kind {
            LogEntryKind::Apply(transition) => match transition.check_applicable(&self.current) {
                Ok(()) => match transition.apply(&mut self.current) {
                    Ok(()) => {
                        self.log.push(entry);
                        Ok(())
                    }
                    Err(err) => {
                        // we need to replay the state
                        // `active` is exactly what it was before this call, and that already
                        // replayed cleanly to produce the old `current` — so re-replaying it here
                        // cannot fail.
                        self.current = Self::replay(&self.log)
                            .expect("previously-active entries must still replay");
                        Err(StateMachineError::Transition(entry.id, err))
                    }
                },
                Err(err) => Err(StateMachineError::Transition(entry.id, err)),
            },
            LogEntryKind::Undo(target) => {
                if !self.contains_id(*target) {
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
                        Err(err)
                    }
                }
            }
        }
    }

    /// Replays every active log entry onto a fresh `S::default()`.
    ///
    /// If the logs contain undos then calling this method is more efficient than applying all
    /// entries one by one with [`Self::apply`] since undone entries will never get applied in the
    /// first place. Note, however that this changes the semantics in case an undone entry would
    /// error. If you call [`Self::apply`] for each entry then it would error before applying the
    /// undo, while [`Self::replay`] fully ignores the undone entry and never applies it and never
    /// surfaces the error.
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
    // already mutating `state` — models a transition whose `apply` breaks the
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

    // Not applicable unless `state` is already at least `self.0` — used to make a
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
            _ => panic!("expected StateMachineError::Transition"),
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
            _ => panic!("expected StateMachineError::Transition"),
        }
        // The failing entry is never recorded in the log. `current` was
        // already mutated by `apply` before it failed though, so per
        // `Transition::apply`'s contract it must now be treated as dirty and
        // not relied upon — recovery means rebuilding from the (unaffected) log.
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
            _ => panic!("expected AppendError::Transition"),
        }
        // The undo itself is rejected: current and the log are unaffected.
        assert_eq!(*sm.current(), 10);
        assert_eq!(sm.log().len(), 2);
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
}

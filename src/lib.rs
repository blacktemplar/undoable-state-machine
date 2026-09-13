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

pub trait Transition<S> {
    /// Must produce the same mutation for the same input state, every time.
    /// May mutate `state` partway before returning `Err` — `StateMachine`
    /// does not rely on failed transitions leaving `state` untouched.
    fn apply(&self, state: &mut S) -> Result<(), TransitionError>;
}

pub struct LogEntry<S> {
    pub id: TransitionId,
    pub kind: LogEntryKind<S>,
}

pub enum LogEntryKind<S> {
    Apply(Box<dyn Transition<S>>),
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

pub struct StateMachine<S> {
    log: Vec<LogEntry<S>>,
    current: S,
}

impl<S: Default> Default for StateMachine<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> StateMachine<S> {
    /// The only way to mutate `current` from outside this crate is through
    /// `append`/`rebuild`; this returns a read-only view.
    pub fn current(&self) -> &S {
        &self.current
    }

    pub fn log(&self) -> &[LogEntry<S>] {
        &self.log
    }

    fn contains_id(&self, id: TransitionId) -> bool {
        self.log.iter().any(|entry| entry.id == id)
    }
}

impl<S: Default> StateMachine<S> {
    pub fn new() -> Self {
        Self {
            log: Default::default(),
            current: Default::default(),
        }
    }

    pub fn build(log: Vec<LogEntry<S>>) -> Result<Self, StateMachineError> {
        let current = Self::replay(&log)?;
        Ok(Self { log, current })
    }

    /// Appends a single entry to the end of the log. This is the local,
    /// speculative path: it does not know about entries other clients may
    /// still be syncing in, and assumes `entry` belongs after everything
    /// already in the log.
    pub fn append(&mut self, entry: LogEntry<S>) -> Result<(), StateMachineError> {
        match &entry.kind {
            LogEntryKind::Apply(transition) => match transition.apply(&mut self.current) {
                Ok(()) => {
                    self.log.push(entry);
                    Ok(())
                }
                Err(err) => {
                    // `active` is exactly what it was before this call, and
                    // that already replayed cleanly to produce the old
                    // `current` — so re-replaying it here cannot fail.
                    self.current = Self::replay(&self.log)
                        .expect("previously-active entries must still replay");
                    Err(StateMachineError::Transition(entry.id, err))
                }
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

    // Replays every active `Apply` entry, in log order, into a fresh
    // `S::default()`. Undoing an entry can uncover a state under which a
    // later, previously-successful entry no longer applies cleanly (e.g. it
    // depended on the undone entry's effect) — callers must not assume this
    // succeeds just because every entry succeeded when first appended.
    fn replay(entries: &[LogEntry<S>]) -> Result<S, StateMachineError> {
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

    impl Transition<i64> for Add {
        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Ok(())
        }
    }

    struct FailAfterMutating(i64);

    impl Transition<i64> for FailAfterMutating {
        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Err("boom".to_string().into())
        }
    }

    // Fails unless `state` is already at least `self.0` — used to make a
    // later entry's success depend on an earlier one still being active.
    struct RequireAtLeast(i64);

    impl Transition<i64> for RequireAtLeast {
        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            if *state < self.0 {
                return Err(format!("need at least {}, have {}", self.0, state).into());
            }
            Ok(())
        }
    }

    fn apply_entry(value: i64) -> LogEntry<i64> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(value))),
        }
    }

    #[test]
    fn append_success_updates_current() {
        let mut sm = StateMachine::<i64>::default();
        sm.append(apply_entry(5)).unwrap();
        sm.append(apply_entry(3)).unwrap();
        assert_eq!(*sm.current(), 8);
    }

    #[test]
    fn append_failure_heals_current_and_does_not_grow_log() {
        let mut sm = StateMachine::<i64>::default();
        sm.append(apply_entry(5)).unwrap();

        let entry = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailAfterMutating(100))),
        };
        let failing_id = entry.id;
        let err = sm.append(entry).unwrap_err();

        match err {
            StateMachineError::Transition(id, err) => {
                assert_eq!(id, failing_id);
                assert_eq!(err.to_string(), "boom");
            }
            _ => panic!("expected AppendError::Transition"),
        }
        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 1);

        sm.append(apply_entry(2)).unwrap();
        assert_eq!(*sm.current(), 7);
    }

    #[test]
    fn undo_cancels_a_prior_apply() {
        let mut sm = StateMachine::<i64>::default();
        let first = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(5))),
        };
        let first_id = first.id;
        sm.append(first).unwrap();
        sm.append(apply_entry(3)).unwrap();
        assert_eq!(*sm.current(), 8);

        sm.append(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(first_id),
        })
        .unwrap();

        assert_eq!(*sm.current(), 3);
    }

    #[test]
    fn undo_of_unknown_target_is_rejected() {
        let mut sm = StateMachine::<i64>::default();
        sm.append(apply_entry(5)).unwrap();

        let err = sm
            .append(LogEntry {
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
        let mut sm = StateMachine::<i64>::default();

        let add = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(10))),
        };
        let add_id = add.id;
        sm.append(add).unwrap();

        // Only succeeds while `add`'s effect is still active.
        let guard = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        };
        let guard_id = guard.id;
        sm.append(guard).unwrap();

        let err = sm
            .append(LogEntry {
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
        let entry = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailAfterMutating(100))),
        };
        let failing_id = entry.id;

        match StateMachine::<i64>::build(vec![entry]) {
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

        match StateMachine::<i64>::build(vec![undo]) {
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

        match StateMachine::<i64>::build(vec![undo_a, a]) {
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

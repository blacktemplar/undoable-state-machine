//! Transitions and helpers to build log entries from them, shared by the tests of several modules.

use crate::{LogEntry, LogEntryKind, StateMachine, Transition, TransitionError, TransitionId};

pub struct Add(pub i64);

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
pub struct FailDuringApply(pub i64);

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
pub struct RequireAtLeast(pub i64);

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
pub struct FailApplyIfBelow(pub i64);

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

pub fn apply_entry(value: i64) -> LogEntry<I64Transition> {
    LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(Add(value))),
    )
}

// Appends a character to the state. Unlike `Add`, pushes don't commute, so
// replaying the active set in the wrong order shows up as a different
// string rather than the same number.
pub struct Push(pub char);

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

pub fn push_entry(c: char) -> LogEntry<StringTransition> {
    LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Push(c))))
}

pub fn undo_entry<T>(target: TransitionId) -> LogEntry<T> {
    LogEntry::new(TransitionId::new(), LogEntryKind::Undo(target))
}

pub fn replace_push_entry(target: TransitionId, c: char) -> LogEntry<StringTransition> {
    LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Replace {
            target,
            with: Box::new(Push(c)),
        },
    )
}

/// Applies `entry` to `sm` and returns its id.
pub fn apply_get_id<S: Default, T: Transition<S>>(
    sm: &mut StateMachine<S, T>,
    entry: LogEntry<T>,
) -> TransitionId {
    let id = entry.id();
    sm.apply(entry).unwrap();
    id
}

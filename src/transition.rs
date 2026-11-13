//! The [`Transition`] trait, which every change a log entry applies implements.

use std::error::Error;

#[cfg(doc)]
use crate::StateMachine;

/// The error [`Transition::check_applicable`] rejects a transition with. Typically a log will
/// contain many different transitions (using `Box<dyn Transition<S> + ...>`, or an enum of them)
/// which can also reject with many different types of errors.
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
///     fn apply(&self, state: &mut i64) {
///         *state += self.0;
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
    /// Only called right after [`Transition::check_applicable`] returned `Ok(())` for the same
    /// `state`, so it cannot fail: whatever could make the transition inapplicable belongs in
    /// that check. It must be deterministic, i.e. the same state must always produce the same
    /// mutation, since the state machine replays the log to undo or replace an entry, and expects
    /// to arrive at the same state as before for every entry that it leaves untouched.
    fn apply(&self, state: &mut S);
}

/// Useful when using `Box<dyn Transition<...> + ...>` as `T` in [`StateMachine`]
impl<S, T: ?Sized + Transition<S>> Transition<S> for Box<T> {
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError> {
        (**self).check_applicable(state)
    }
    fn apply(&self, state: &mut S) {
        (**self).apply(state);
    }
}

//! The [`Transition`] trait, which every change a log entry applies implements.

use std::error::Error;

#[cfg(doc)]
use crate::StateMachine;

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

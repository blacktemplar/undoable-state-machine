//! (De)serialization of a [`StateMachine`] as its log.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(doc)]
use crate::StateMachineError;
use crate::{LogEntry, StateMachine, Transition};

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

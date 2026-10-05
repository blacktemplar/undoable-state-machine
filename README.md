# undoable-state-machine

A small, generic library for a state machine whose transitions are applied through an append-only
log and can be selectively undone and redone by referencing a specific past transition's id, not
just the most recent one.

The log never physically removes anything: undoing a transition is itself just another log entry,
targeting the id of the entry it undoes. Because undo entries are ordinary log entries, they can be
undone too. Undoing an undo reactivates the original transition, giving redo for free without a
separate mechanism. Undoing a transition that isn't the most recent one is rejected if some later,
still-active transition depends on it, so the state machine never silently replays into a broken
state.

The crate has no notion of any particular domain: it is generic over the state type `S` and the
transition type `T: Transition<S>`, and consumers supply their own.

## Example

```rust
use undoable_state_machine::{
    LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    TransitionId,
};

struct Add(i64);

impl Transition<i64> for Add {
    fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
        Ok(())
    }

    fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
        *state += self.0;
        Ok(())
    }
}

let mut sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();

let first_id = TransitionId::new();
sm.apply(LogEntry::new(first_id, LogEntryKind::Apply(Box::new(Add(5)))))?;
sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(3)))))?;
assert_eq!(*sm.current(), 8);

// Undo the first transition, the second transition survives on top of the new active set.
sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(first_id)))?;
assert_eq!(*sm.current(), 3);

// `StateMachine::build` reconstructs a state machine from a previously
// recorded log, e.g. one persisted elsewhere.
let persisted: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(1)))),
    LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(2)))),
];
let restored = StateMachine::<i64, Box<dyn Transition<i64>>>::build(persisted)?;
assert_eq!(*restored.current(), 3);
# Ok::<(), StateMachineError>(())
```

## Serialization

The `serde` feature implements `Serialize`/`Deserialize` for `TransitionId`, `LogEntry`,
`LogEntryKind` and `StateMachine`:

```toml
[dependencies]
undoable-state-machine = { version = "0.1", features = ["serde"] }
```

A whole log persists, undo entries included, so a restored state machine can still undo (and
undo its undos). A `StateMachine` serializes as its log alone - `current()` is derived state and
is recomputed by `build` on deserialization, which is also where a log that doesn't replay
cleanly is rejected. See the `Serialize for StateMachine` impl for a full example.

The log types are (de)serializable exactly when the transition type `T` is, which
`Box<dyn Transition<S>>` isn't: use an enum of the transitions, or a crate like
[`typetag`](https://docs.rs/typetag), as `T` for a persistable log.

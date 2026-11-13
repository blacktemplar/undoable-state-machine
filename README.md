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

A past transition can also be replaced with a different one by another log entry. The
replacement runs at the position of the transition it replaces, and undoing it brings back the
previous version.

For undo and redo as in an editor, a `SessionHistory` keeps track of the changes one session made,
and undoes and redoes them in the order it made them, as ordinary undo entries. A change another
session appended to the same log, e.g. through a sync, is never undone by it; if such a change
makes an undo (redo) fail, the history has nothing more to undo (redo).

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

    fn apply(&self, state: &mut i64) {
        *state += self.0;
    }
}

let mut sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();

// `apply_new` appends a transition with a fresh id, and returns that id
let first_id = sm.apply_new(Box::new(Add(5)))?;
sm.apply_new(Box::new(Add(3)))?;
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

## Cost

The design favours simplicity over speed of undo. Applying a transition runs it once, on the
current state. Every undo and every replacement, though, replays all transitions in effect from
the default state, and every undo also works out again which entries of the whole log are in
effect, so each costs time linear in the length of the log, plus whatever the transitions
themselves cost. `StateMachine::build` replays a log once. That is fine for logs of the size
people build by hand, but makes undo slower the longer the log grows.

## Serialization

The `serde` feature implements `Serialize`/`Deserialize` for `TransitionId`, `LogEntry`,
`LogEntryKind` and `StateMachine`:

```toml
[dependencies]
undoable-state-machine = { version = "0.3", features = ["serde"] }
```

A whole log persists, undo entries included, so a restored state machine can still undo (and
undo its undos). A `StateMachine` serializes as its log alone - `current()` is derived state and
is recomputed by `build` on deserialization, which is also where a log that doesn't replay
cleanly is rejected. See the `Serialize for StateMachine` impl for a full example.

The log types are (de)serializable exactly when the transition type `T` is, which
`Box<dyn Transition<S>>` isn't: use an enum of the transitions, or a crate like
[`typetag`](https://docs.rs/typetag), as `T` for a persistable log.

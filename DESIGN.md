# undoable-state-machine — Design

A small, generic library for a state machine whose transitions are applied
through an append-only log and can be selectively undone — and redone —
by referencing a specific past transition's id, not just the most recent
one. The crate has no notion of any particular domain; it is generic over
the state type and the error type a transition can fail with, and any
consumer supplies its own state, error, and transition types.

## Requirements

- Must support undoing a specific past transition by id, even when it
  isn't the most recently applied one. Transitions may arrive from
  multiple independent sources whose submissions interleave in arrival
  order, so a caller cannot rely on "the previous transition" meaning
  anything stable — the transition it wants undone must be addressable by
  the id it was given when it was originally applied, not by relative
  position.
- Undoing must itself be represented as a state transition, addressable
  by its own id — including being undoable itself. Determining which
  transitions are currently in effect therefore means scanning the log
  from the most recent entry backward: whether an entry counts depends on
  whether anything later in the log undid it, which itself depends on
  whether *that* undo was, in turn, undone, and so on toward the front.
- Undoing a transition that isn't at the tail must not silently corrupt
  state if some later, still-active transition depended on it (e.g. it
  operates on an entity the undone transition created) — this must be
  caught explicitly rather than producing a broken replay.
- Must work uniformly for any `Transition` implementation regardless of
  which crate defines it — this crate must not need to know about, or be
  modified for, a specific consumer's transition types. In particular,
  undo must not require every transition author to also hand-write a
  correct inverse.
- Must remain safe to combine indefinitely with further transitions
  (including further undos) afterward.

## Proposed Solution

The log is append-only — nothing is ever physically removed — and undo is
just another kind of entry in it:

```rust
pub struct TransitionId(u64); // monotonic, assigned when an entry is appended

pub trait Transition<S, E> {
    /// Must produce the same mutation for the same input state, every time.
    /// May mutate `state` partway before returning `Err` — `StateMachine`
    /// does not rely on failed transitions leaving `state` untouched.
    fn apply(&self, state: &mut S) -> Result<(), E>;
}

enum LogEntry<S, E> {
    Apply(Box<dyn Transition<S, E>>),
    Undo(TransitionId), // targets an earlier entry's id
}

pub struct StateMachine<S, E> {
    log: Vec<(TransitionId, LogEntry<S, E>)>,
    current: S, // private field; only `apply`/`undo` (below) ever get `&mut`
}
```

There is deliberately no `initial: S` field. The whole point of keeping an
append-only log is that it is the full record of how `current` came to be
— an `initial` snapshot supplied at construction time would be exactly
the kind of fact about `current` that the log *doesn't* explain, which
undermines the same audit property the log's append-only-ness exists to
give (see the rejected "physically excising" alternative below). So every
replay — on `undo`, and on `apply`'s error path — starts from `S::default()`
rather than a stored/cloned `initial`. This also drops the `S: Clone` bound
from the whole crate in favor of `S: Default`: nothing is ever cloned,
only (re)computed from nothing and mutated forward.

`apply` mutates `state` in place rather than returning a new `S`. Some
consumers' states are large with only a small part changing per
transition, so rebuilding the whole value on every step would be wasteful.
`current` is a private field, so ordinary Rust visibility already gives
the guarantee needed here: only code inside this crate can ever obtain
`&mut self.current`, and `StateMachine` only does so from its own
apply/replay logic (below). Outside the crate, the sole way to observe
`current` is the `pub fn current(&self) -> &S` accessor, and a
`Transition` impl never sees `current` itself — only the single `&mut S`
it's handed for the duration of its own `apply` call. No wrapper type is
needed to enforce this; it falls out of `current` simply not being `pub`.

**Optimistic apply, heal on error.** `StateMachine::apply` — the public
method a caller uses to add a transition — is a different method from
`Transition::apply`; it's the only code that ever hands out `&mut
self.current` to the latter. It mutates `current` directly rather
than through a scratch clone; it's the hot path, and the expected case
is that the transition's `apply` succeeds, so there's nothing extra to
pay for most of the time. The risk is that when `apply` *does* return
`Err`, it may have already mutated `state` partway (the trait doesn't
require otherwise — requiring every implementation, including
third-party ones, to hand-write a correct "no effect on failure" guarantee
was rejected as the same kind of burden undo already avoids placing on
transition authors). So on `Err`, `current` is no longer trustworthy and
is recomputed from scratch: replay every still-active entry, in log order,
into a fresh `S::default()` — the same replay `undo` uses below — and
install that as `current` before returning the error to the caller. This
recomputation itself cannot fail, because every entry being replayed was
already active and successfully applied before this call started, and
`apply` is required to be deterministic. The failed transition is never
appended to the log, so it plays no part in this recomputation.

`undo` can't use the cheap direct-mutation path at all, because it has to
test a *hypothetical* active set before committing to it — the target
being undone might invalidate something later, and discovering that must
not touch `current` for real. So `undo` always replays into a fresh
`S::default()` first and only swaps the result in as `current` once
the entire replay has succeeded, leaving `current` and the log untouched
on failure.

**Computing what's active.** Because an `Undo` can itself be undone, "is
this entry in effect" isn't just "was it ever undone" — it's "was it
undone by something that is *itself* still active." That's resolved by
walking the log from the back to the front, maintaining a
`cancelled: HashSet<TransitionId>`:

- An entry is active iff its own id isn't already in `cancelled` (i.e.
  nothing later and still-active targeted it).
- If an active entry is `Undo(target)`, add `target` to `cancelled`.
- If an entry isn't active, it has no effect at all — so if it's an
  `Undo`, it does *not* cancel its target either.

This gives "undo of an undo" as redo for free: `Undo(U)` where `U` is
itself `Undo(T)` — if the new undo is active, it cancels `U`, which means
`U` no longer cancels `T`, so `T` becomes active again. No separate redo
mechanism is needed. `current` is then obtained by taking a fresh
`S::default()` and mutating it in place by applying every
active `Apply` entry, in log order (`Undo` entries are no-ops for `apply`
— their effect is structural, handled by the active-set computation
above, not by mutating state themselves).

**Validation before commit.** `undo(target: TransitionId)` must check,
before it takes effect, that the resulting active set still replays
cleanly — a later active transition may have depended on the one being
targeted. Recompute the active set as if the new `Undo` entry were
appended, and replay forward starting from a fresh `S::default()`; only if
the whole replay succeeds does the state machine actually append the
entry and swap that result in as the new `current`. If any later
active transition's `apply` now fails, reject with an error naming the
first transition that broke, and leave the log/`current` untouched —
because the replay only ever mutated the scratch value, not `current`,
this gives the same atomicity guarantee whether the target is at the tail
or buried in the middle.

```rust
impl<S: Default, E> StateMachine<S, E> {
    pub fn apply(&mut self, transition: Box<dyn Transition<S, E>>) -> Result<TransitionId, E>;
    pub fn undo(&mut self, target: TransitionId) -> Result<TransitionId, UndoError<E>>;
    pub fn undo_last(&mut self) -> Result<TransitionId, UndoError<E>>; // undoes whatever is currently the most recent active entry

    pub fn current(&self) -> &S;
}

pub enum UndoError<E> {
    UnknownTransitionId(TransitionId),
    WouldInvalidate(TransitionId, E),
}
```

`undo_last` is a convenience for the common case where "most recent" is
still meaningful to the caller — it looks up the most recent active entry
at call time and calls `undo` on its id — but `undo(id)` is the primitive
callers should use once transitions can arrive out of order.

Recomputing the active set and replaying is O(log length) per call, which
is simplest for v1. If profiling later shows this is too slow or
memory-heavy at typical log sizes, periodic snapshots (recompute from the
nearest snapshot's active set + state instead of from `S::default()`) can
be added additively, without changing this API.

## Alternatives

- **Per-transition inverse** (command pattern: each `Transition` also
  implements `fn invert(&self, before: &S) -> Box<dyn Transition<S, E>>`),
  undo = apply the inverse directly. This is what the existing Rust
  `undo`/`redo` crates do. *Advantage:* doesn't need a replay of
  everything after the target. *Disadvantage:* fundamentally shaped
  around undoing from the tail, immediately after apply — an inverse for
  transition `A` has no way to account for unrelated transitions stacked
  on top of `A` by the time it's undone, so it doesn't generalize to
  "undo an arbitrary past transition while newer ones survive" at all. It
  also still requires every transition author, including third parties,
  to hand-write a correct inverse. Rejected.
- **Boolean `undone` flag stored directly on the transition** instead of
  a separate `Undo` log entry (a pattern seen in some event-sourced
  systems). *Advantage:* slightly less indirection than scanning for
  `Undo` entries. *Disadvantage:* the flag itself would need to be
  mutable/undoable to satisfy "undo is itself undoable," which just
  reintroduces the same backward-chasing problem one level down (now on
  the flag instead of on a log entry) without the benefit of the flag
  change being uniformly represented as a `Transition` in the same log.
  Rejected in favor of representing undo the same way as everything else.
- **Physically excising the target entry from the log and replaying the
  remainder**, an earlier iteration of this design. *Advantage:* no
  bookkeeping for cancelled ids. *Disadvantage:* loses the history of what
  was undone (can't tell "never happened" apart from "happened and was
  undone", which matters for audit/debugging), and gives no natural way to
  undo an undo (there's nothing left in the log to target). Superseded by
  the append-only design above.

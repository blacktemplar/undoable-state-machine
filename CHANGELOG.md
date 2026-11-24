# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-11-24

### Added

- Metadata on every log entry: `LogEntry<T, M = ()>` holds a `meta: M` (`LogEntry::meta`), and
  `StateMachine<S, T, M = ()>` keeps entries of that type. The state machine never reads it and
  never makes it up, e.g. a consumer records when an entry was appended.

### Changed

- **Breaking:** every operation appending an entry takes its metadata: `LogEntry::new(id, kind,
  meta)`, `StateMachine::apply_new(transition, meta)`, `SessionHistory::apply_new(machine,
  transition, meta)`, and `SessionHistory::undo_last(machine, meta)` and `redo_last(machine,
  meta)` for each undo entry they append (so `M: Clone`). The undos `SessionHistory::apply_all`
  appends after a rejected entry carry that entry's metadata. Pass `()` to record nothing.
- **Breaking:** with the `serde` feature, an entry serializes as `{"id", "kind", "meta"}`. A log
  serialized by an earlier version has no `meta` and has to be converted before it is read.

## [0.4.0] - 2026-11-16

### Added

- `SessionHistory::apply_all`: applies several entries as one action of the session, which
  `undo_last` undoes and `redo_last` redoes as a whole. If one of them is rejected, those applied
  before it are undone again, so the state and the history are as before. Logs are unaffected:
  which entries form an action is not part of the log.

### Changed

- **Breaking:** an action of a `SessionHistory` is a group of entries. `undo_last` and
  `redo_last` return the ids of every entry they append (`Vec<TransitionId>`), and `next_undo`
  and `next_redo` the ids of the entries of the action (`Option<&[TransitionId]>`), `None` if
  another session has undone or redone any of them. An undo or redo rejected part of the way
  through an action of several entries leaves the entries appended before in the log, and ends
  the history as any failed undo or redo does.

## [0.3.0] - 2026-11-13

### Added

- `StateMachine::apply_new` and `SessionHistory::apply_new`: apply a transition as a new `Apply`
  with a fresh `TransitionId`, and return that id.
- `StateMachine::rev_in_effect_applied`: the `Apply` entries in effect, newest first, each as its
  id and the transition that runs in its place.
- A section on the cost of undo and replace in the README.

### Changed

- **Breaking:** `Transition::apply` returns nothing: it runs only after `check_applicable`
  accepted the same state, so whatever could make it fail belongs in that check. The recovery
  `StateMachine::apply` did when `apply` failed is gone, and with it its panic.
  `StateMachineError::Transition` now means that `check_applicable` rejected a transition. Logs
  are unaffected.

## [0.2.0] - 2026-11-12

### Added
- `SessionHistory::next_undo` and `next_redo`: the action undoing (redoing)
  acts on next, unless another session has undone (redone) it already.

### Changed

- A `SessionHistory` whose undo fails because of another session's entry
  (`UndoLastError::UndoneElsewhere` or `Rejected`) is left with nothing to
  undo, and one whose redo fails so with nothing to redo, rather than keeping
  the action to try again; what it can redo (undo) stays.

## [0.1.0] - 2026-09-15

### Added

- Initial release: a generic, undoable state machine backed by an
  append-only log (`StateMachine`, `Transition`, `LogEntry`).

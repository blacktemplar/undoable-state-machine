# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

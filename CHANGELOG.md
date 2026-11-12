# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

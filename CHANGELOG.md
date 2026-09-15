# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-09-15

### Added

- Initial release: a generic, undoable state machine backed by an
  append-only log (`StateMachine`, `Transition`, `LogEntry`).
- `Transition::check_applicable`/`apply` split, with automatic healing by
  log replay on `apply` failure.
- Undo of a past transition via `LogEntryKind::Undo`, with rejection of
  undos that would break a later, still-active dependent entry.
- Undo-of-undo as redo.
- `StateMachine::build` to reconstruct a state machine from a persisted log.

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Optional `serde` feature: `Serialize`/`Deserialize` for `TransitionId`,
  `LogEntry`, `LogEntryKind` and `StateMachine`, so a whole log (undo
  entries included) can be persisted and restored. A `StateMachine`
  serializes as its log and is rebuilt through `StateMachine::build` on
  deserialization.
- `StateMachine::rev_in_effect`: the log entries that are in effect, i.e.
  not cancelled by an undo in effect, lazily from the newest to the oldest.

### Fixed

- `StateMachine::replay` applied the active log entries in reverse order,
  so `StateMachine::build` and every `Undo` replayed the log backwards.
  Only visible with transitions that don't commute.

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

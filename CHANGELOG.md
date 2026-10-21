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
- `StateMachine::entry`: the log entry with a given id, in constant time.
- `LogEntryKind::Replace`: replaces the transition of a previous `Apply`,
  running at that entry's position in the log. Replacing again targets the
  same `Apply`, the latest replacement in effect wins, so undoing it falls
  back to the previous one. `StateMachine::apply` rejects replacing an
  undone `Apply` (`StateMachineError::ReplaceTargetNotInEffect`), while
  `StateMachine::build` accepts it as a replacement without effect until
  the `Apply` is redone. Replacing anything but an `Apply` is rejected with
  `StateMachineError::ReplaceTargetNotApply`.
- `StateMachine::effective_transition`: the transition that runs in place of
  an `Apply`, taking replacements into account.

### Changed

- `StateMachineError::UnknownTarget`'s message no longer mentions undo, as a
  replace can target an unknown id too.

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

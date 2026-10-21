use proptest::strategy::Strategy;

use super::{StateMachine, effects::EntryEffect};
use crate::{
    LogEntry, LogEntryKind, StateMachineError, Transition, TransitionId,
    test_util::{
        Add, FailApplyIfBelow, FailDuringApply, I64Transition, Push, RequireAtLeast,
        StringTransition, apply_entry, apply_get_id, push_entry, replace_push_entry, undo_entry,
    },
};

#[test]
fn build_applies_active_transitions_in_log_order() {
    let sm = StateMachine::<String, StringTransition>::build(vec![
        push_entry('a'),
        push_entry('b'),
        push_entry('c'),
    ])
    .unwrap();

    assert_eq!(sm.current(), "abc");
}

#[test]
fn undo_keeps_the_surviving_transitions_in_log_order() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x = push_entry('x');
    let x_id = x.id();
    sm.apply(x).unwrap();
    sm.apply(push_entry('b')).unwrap();
    assert_eq!(sm.current(), "axb");

    sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(x_id)))
        .unwrap();

    assert_eq!(sm.current(), "ab");
}

#[test]
fn build_reactivates_an_undone_entry_in_its_original_slot() {
    let a = push_entry('a');
    let x = push_entry('x');
    let x_id = x.id();
    let b = push_entry('b');
    let undo_x = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(x_id));
    let undo_x_id = undo_x.id();
    let undo_undo_x = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo_x_id));

    let undone = StateMachine::build(vec![a, x, b, undo_x]).unwrap();
    assert_eq!(undone.current(), "ab");

    let a = push_entry('a');
    let x = LogEntry::new(
        x_id,
        LogEntryKind::Apply(Box::new(Push('x')) as StringTransition),
    );
    let b = push_entry('b');
    let undo_x = LogEntry::new(undo_x_id, LogEntryKind::Undo(x_id));
    let redone = StateMachine::build(vec![a, x, b, undo_x, undo_undo_x]).unwrap();

    // `x` is active again, back in its original position - not appended at the end.
    assert_eq!(redone.current(), "axb");
}

#[test]
fn append_success_updates_current() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    sm.apply(apply_entry(5)).unwrap();
    sm.apply(apply_entry(3)).unwrap();
    assert_eq!(*sm.current(), 8);
}

#[test]
fn append_rejects_a_not_applicable_transition_without_mutating_state() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    sm.apply(apply_entry(5)).unwrap();

    let entry: LogEntry<I64Transition> = LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
    );
    let failing_id = entry.id();
    let err = sm.apply(entry).unwrap_err();

    match err {
        StateMachineError::Transition(id, _) => assert_eq!(id, failing_id),
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    assert_eq!(*sm.current(), 5);
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn append_apply_failure_does_not_grow_log() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    sm.apply(apply_entry(5)).unwrap();

    let entry: LogEntry<I64Transition> = LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(FailDuringApply(100))),
    );
    let failing_id = entry.id();
    let err = sm.apply(entry).unwrap_err();

    match err {
        StateMachineError::Transition(id, err) => {
            assert_eq!(id, failing_id);
            assert_eq!(err.to_string(), "boom");
        }
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    // The failing entry is never recorded in the log. `current` was
    // already mutated by `apply` before it failed though, so per
    // `Transition::apply`'s contract it must now be treated as dirty and
    // not relied upon. Recovery means rebuilding from the (unaffected) log.
    assert_eq!(sm.log().len(), 1);

    let rebuilt = StateMachine::<i64, I64Transition>::build(vec![apply_entry(5)]).unwrap();
    assert_eq!(*rebuilt.current(), 5);
}

#[test]
fn undo_cancels_a_prior_apply() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let first: LogEntry<I64Transition> =
        LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(5))));
    let first_id = first.id();
    sm.apply(first).unwrap();
    sm.apply(apply_entry(3)).unwrap();
    assert_eq!(*sm.current(), 8);

    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Undo(first_id),
    ))
    .unwrap();

    assert_eq!(*sm.current(), 3);
}

#[test]
fn undo_of_unknown_target_is_rejected() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    sm.apply(apply_entry(5)).unwrap();

    let err = sm
        .apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Undo(TransitionId::new()),
        ))
        .unwrap_err();

    assert!(matches!(err, StateMachineError::UnknownTarget(_)));
    assert_eq!(*sm.current(), 5);
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn undo_that_breaks_a_later_entry_is_rejected_and_blames_that_entry() {
    let mut sm = StateMachine::<i64, I64Transition>::default();

    let add: LogEntry<I64Transition> =
        LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(10))));
    let add_id = add.id();
    sm.apply(add).unwrap();

    // Only succeeds while `add`'s effect is still active.
    let guard: LogEntry<I64Transition> = LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
    );
    let guard_id = guard.id();
    sm.apply(guard).unwrap();

    let undo_id = TransitionId::new();
    let err = sm
        .apply(LogEntry::new(undo_id, LogEntryKind::Undo(add_id)))
        .unwrap_err();

    match err {
        StateMachineError::Transition(id, _) => assert_eq!(id, guard_id),
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    // The undo itself is rejected: current and the log are unaffected.
    assert_eq!(*sm.current(), 10);
    assert_eq!(sm.log().len(), 2);
    assert!(sm.entry(undo_id).is_none());
}

#[test]
fn undo_that_breaks_a_later_entrys_apply_is_rejected_and_blames_that_entry() {
    let mut sm = StateMachine::<i64, I64Transition>::default();

    let add: LogEntry<I64Transition> =
        LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(10))));
    let add_id = add.id();
    sm.apply(add).unwrap();

    // Succeeds now, while `add`'s effect keeps `state` at least 10, but
    // its `check_applicable` doesn't encode that dependency - only its
    // `apply` does.
    let guard: LogEntry<I64Transition> = LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(FailApplyIfBelow(10))),
    );
    let guard_id = guard.id();
    sm.apply(guard).unwrap();
    assert_eq!(*sm.current(), 11);

    let err = sm
        .apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Undo(add_id),
        ))
        .unwrap_err();

    match err {
        StateMachineError::Transition(id, _) => assert_eq!(id, guard_id),
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    // The undo itself is rejected: current and the log are unaffected.
    assert_eq!(*sm.current(), 11);
    assert_eq!(sm.log().len(), 2);
}

#[test]
fn apply_rejects_a_duplicate_id() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let entry = apply_entry(5);
    let id = entry.id();
    sm.apply(entry).unwrap();

    let dup: LogEntry<I64Transition> = LogEntry::new(id, LogEntryKind::Apply(Box::new(Add(3))));
    let err = sm.apply(dup).unwrap_err();

    match err {
        StateMachineError::DuplicateId(dup_id) => assert_eq!(dup_id, id),
        other => panic!("expected StateMachineError::DuplicateId, got {other:?}"),
    }
    assert_eq!(*sm.current(), 5);
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn apply_rejects_an_undo_with_a_duplicate_id() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let entry = apply_entry(5);
    let id = entry.id();
    sm.apply(entry).unwrap();

    let dup: LogEntry<I64Transition> = LogEntry::new(id, LogEntryKind::Undo(id));
    let err = sm.apply(dup).unwrap_err();

    match err {
        StateMachineError::DuplicateId(dup_id) => assert_eq!(dup_id, id),
        other => panic!("expected StateMachineError::DuplicateId, got {other:?}"),
    }
    assert_eq!(*sm.current(), 5);
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn build_rejects_a_duplicate_id() {
    let a = apply_entry(5);
    let id = a.id();
    let b: LogEntry<I64Transition> = LogEntry::new(id, LogEntryKind::Apply(Box::new(Add(3))));

    match StateMachine::<i64, I64Transition>::build(vec![a, b]) {
        Err(StateMachineError::DuplicateId(dup_id)) => assert_eq!(dup_id, id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::DuplicateId, got {other:?}"),
    }
}

#[test]
fn build_computes_current_from_a_log() {
    let a = apply_entry(5);
    let a_id = a.id();
    let b = apply_entry(3);
    let undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a_id));

    let sm = StateMachine::build(vec![a, b, undo_a]).unwrap();

    assert_eq!(*sm.current(), 3);
    assert_eq!(sm.log().len(), 3);
}

#[test]
fn build_with_undone_undo_computes_current() {
    let a_id = TransitionId::new();
    let a = LogEntry::new(a_id, LogEntryKind::Apply(Box::new(Add(5))));
    let undo_a_id = TransitionId::new();
    let undo_a = LogEntry::new(undo_a_id, LogEntryKind::Undo(a_id));
    let undo_undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo_a_id));

    let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

    assert_eq!(*sm.current(), 5);
    assert_eq!(sm.log().len(), 3);
}

#[test]
fn build_surfaces_the_failing_transition() {
    let entry: LogEntry<I64Transition> = LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(FailDuringApply(100))),
    );
    let failing_id = entry.id();

    match StateMachine::<i64, I64Transition>::build(vec![entry]) {
        Err(StateMachineError::Transition(id, err)) => {
            assert_eq!(id, failing_id);
            assert_eq!(err.to_string(), "boom");
        }
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
}

#[test]
fn build_rejects_undo_of_an_unknown_target() {
    let unknown_id = TransitionId::new();
    let undo = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(unknown_id));

    match StateMachine::<i64, I64Transition>::build(vec![undo]) {
        Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, unknown_id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
    }
}

#[test]
fn build_rejects_an_undone_undo_of_an_unknown_target() {
    let unknown_id = TransitionId::new();
    let undo = undo_entry::<I64Transition>(unknown_id);
    let undo_undo = undo_entry(undo.id());

    match StateMachine::<i64, I64Transition>::build(vec![undo, undo_undo]) {
        Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, unknown_id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
    }
}

#[test]
fn build_rejects_undo_appearing_before_its_target() {
    let a = apply_entry(5);
    let a_id = a.id();
    let undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a_id));

    match StateMachine::<i64, I64Transition>::build(vec![undo_a, a]) {
        Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, a_id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
    }
}

#[test]
fn build_allows_undoing_an_undo_to_reactivate_the_original() {
    let a = apply_entry(5);
    let a_id = a.id();
    let undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a_id));
    let undo_a_id = undo_a.id();
    let undo_undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo_a_id));

    let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

    assert_eq!(*sm.current(), 5);
}

#[test]
fn rev_in_effect_skips_an_undone_undo_but_not_its_target() {
    let a = apply_entry(5);
    let a_id = a.id();
    let undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a_id));
    let undo_a_id = undo_a.id();
    let undo_undo_a = LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo_a_id));
    let undo_undo_a_id = undo_undo_a.id();

    let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

    let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
    assert_eq!(in_effect, [undo_undo_a_id, a_id]);
}

#[test]
fn replace_runs_in_the_slot_of_its_target() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    sm.apply(push_entry('b')).unwrap();

    sm.apply(replace_push_entry(x_id, 'y')).unwrap();

    assert_eq!(sm.current(), "ayb");
}

#[test]
fn undoing_the_latest_replace_falls_back_to_the_previous_one() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    sm.apply(push_entry('b')).unwrap();
    let y_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'y'));
    let z_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'z'));
    assert_eq!(sm.current(), "azb");

    let undo_z_id = apply_get_id(&mut sm, undo_entry(z_id));
    assert_eq!(sm.current(), "ayb");
    sm.apply(undo_entry(y_id)).unwrap();
    assert_eq!(sm.current(), "axb");
    // redoing `z` brings it back on top of the (still undone) `y`
    sm.apply(undo_entry(undo_z_id)).unwrap();
    assert_eq!(sm.current(), "azb");
}

#[test]
fn undoing_a_shadowed_replace_changes_nothing_until_the_later_one_is_undone() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    let y_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'y'));
    let z_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'z'));

    sm.apply(undo_entry(y_id)).unwrap();
    assert_eq!(sm.current(), "az");
    sm.apply(undo_entry(z_id)).unwrap();
    assert_eq!(sm.current(), "ax");
}

#[test]
fn undoing_the_target_undoes_it_with_its_replacements() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    sm.apply(replace_push_entry(x_id, 'y')).unwrap();

    let undo_x_id = apply_get_id(&mut sm, undo_entry(x_id));
    assert_eq!(sm.current(), "a");
    // redoing `x` brings it back replaced
    sm.apply(undo_entry(undo_x_id)).unwrap();
    assert_eq!(sm.current(), "ay");
}

#[test]
fn replace_can_satisfy_a_later_entry_that_undoing_its_target_would_break() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let add_id = apply_get_id(&mut sm, apply_entry(10));
    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
    ))
    .unwrap();

    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Replace {
            target: add_id,
            with: Box::new(Add(20)),
        },
    ))
    .unwrap();

    assert_eq!(*sm.current(), 20);
}

#[test]
fn replace_that_breaks_a_later_entry_is_rejected_and_blames_that_entry() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let add_id = apply_get_id(&mut sm, apply_entry(10));
    let guard_id = apply_get_id(
        &mut sm,
        LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        ),
    );

    let replace_id = TransitionId::new();
    let err = sm
        .apply(LogEntry::new(
            replace_id,
            LogEntryKind::Replace {
                target: add_id,
                with: Box::new(Add(1)),
            },
        ))
        .unwrap_err();

    match err {
        StateMachineError::Transition(id, _) => assert_eq!(id, guard_id),
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    assert_eq!(*sm.current(), 10);
    assert_eq!(sm.log().len(), 2);
    assert!(sm.entry(replace_id).is_none());
}

#[test]
fn a_failing_replacement_is_rejected_and_blames_the_replace() {
    let mut sm = StateMachine::<i64, I64Transition>::default();
    let add_id = apply_get_id(&mut sm, apply_entry(10));

    let replace_id = TransitionId::new();
    let err = sm
        .apply(LogEntry::new(
            replace_id,
            LogEntryKind::Replace {
                target: add_id,
                with: Box::new(FailDuringApply(1)),
            },
        ))
        .unwrap_err();

    match err {
        StateMachineError::Transition(id, err) => {
            assert_eq!(id, replace_id);
            assert_eq!(err.to_string(), "boom");
        }
        other => panic!("expected StateMachineError::Transition, got {other:?}"),
    }
    assert_eq!(*sm.current(), 10);
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn apply_rejects_a_replace_of_an_unknown_target() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();

    let unknown_id = TransitionId::new();
    match sm.apply(replace_push_entry(unknown_id, 'y')) {
        Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, unknown_id),
        other => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
    }
    assert_eq!(sm.log().len(), 1);
}

#[test]
fn apply_rejects_a_replace_of_a_non_apply() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    let replace_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'y'));
    let undo_id = apply_get_id(&mut sm, undo_entry(replace_id));

    for target in [replace_id, undo_id] {
        match sm.apply(replace_push_entry(target, 'z')) {
            Err(StateMachineError::ReplaceTargetNotApply(id)) => assert_eq!(id, target),
            other => panic!("expected StateMachineError::ReplaceTargetNotApply, got {other:?}"),
        }
    }
    assert_eq!(sm.current(), "x");
    assert_eq!(sm.log().len(), 3);
}

#[test]
fn apply_rejects_a_replace_of_an_undone_target() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    sm.apply(push_entry('a')).unwrap();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    sm.apply(undo_entry(x_id)).unwrap();

    match sm.apply(replace_push_entry(x_id, 'y')) {
        Err(StateMachineError::ReplaceTargetNotInEffect(id)) => assert_eq!(id, x_id),
        other => panic!("expected StateMachineError::ReplaceTargetNotInEffect, got {other:?}"),
    }
    assert_eq!(sm.current(), "a");
    assert_eq!(sm.log().len(), 3);
}

#[test]
fn build_accepts_a_replace_of_an_undone_target_which_runs_once_redone() {
    let a = push_entry('a');
    let x = push_entry('x');
    let x_id = x.id();
    let undo_x = undo_entry(x_id);
    let undo_x_id = undo_x.id();
    let replace_x = replace_push_entry(x_id, 'y');
    let replace_x_id = replace_x.id();

    let mut sm = StateMachine::build(vec![a, x, undo_x, replace_x]).unwrap();
    assert_eq!(sm.current(), "a");
    // the dormant replace is in effect nonetheless
    let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
    assert_eq!(in_effect[..2], [replace_x_id, undo_x_id]);

    sm.apply(undo_entry(undo_x_id)).unwrap();
    assert_eq!(sm.current(), "ay");
}

#[test]
fn build_rejects_a_replace_of_a_non_apply() {
    let x = push_entry('x');
    let replace_x = replace_push_entry(x.id(), 'y');
    let replace_id = replace_x.id();
    let replace_replace = replace_push_entry(replace_id, 'z');

    match StateMachine::build(vec![x, replace_x, replace_replace]) {
        Err(StateMachineError::ReplaceTargetNotApply(id)) => assert_eq!(id, replace_id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => {
            panic!("expected StateMachineError::ReplaceTargetNotApply, got {other:?}")
        }
    }
}

#[test]
fn build_rejects_a_replace_appearing_before_its_target() {
    let x = push_entry('x');
    let x_id = x.id();
    let replace_x = replace_push_entry(x_id, 'y');

    match StateMachine::build(vec![replace_x, x]) {
        Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, x_id),
        Ok(_) => panic!("expected build to fail"),
        Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
    }
}

#[test]
fn rev_in_effect_keeps_a_replaced_apply_and_its_shadowed_replacements() {
    let mut sm = StateMachine::<String, StringTransition>::default();
    let x_id = apply_get_id(&mut sm, push_entry('x'));
    let y_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'y'));
    let z_id = apply_get_id(&mut sm, replace_push_entry(x_id, 'z'));

    let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
    assert_eq!(in_effect, [z_id, y_id, x_id]);
}

/// What a random `apply` call in the property test below does: either
/// append a new `Add`, or undo or replace a previously-appended entry
/// (picked by index into `all_ids`, wrapped so every index is valid).
#[derive(Clone, Debug)]
enum RandomOp {
    Apply(i64),
    Undo(usize),
    Replace(usize, i64),
}

enum MirrorKind {
    Apply(i64),
    Undo(TransitionId),
    Replace(TransitionId, i64),
}

proptest::proptest! {
    // The crate's core invariant: `current()` is always exactly what
    // you'd get by replaying the log's active set from scratch. We
    // check it after every step of a random sequence of applies/undos,
    // rebuilding independently via `StateMachine::build` from a mirror
    // of the entries the state machine actually accepted.
    #[test]
    fn current_matches_a_from_scratch_replay_of_the_log(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                (-1_000_000i64..1_000_000i64).prop_map(RandomOp::Apply),
                proptest::prelude::any::<usize>().prop_map(RandomOp::Undo),
                (proptest::prelude::any::<usize>(), -1_000_000i64..1_000_000i64)
                    .prop_map(|(idx, v)| RandomOp::Replace(idx, v)),
            ],
            0..50,
        )
    ) {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        let mut mirror: Vec<(TransitionId, MirrorKind)> = Vec::new();
        let mut all_ids: Vec<TransitionId> = Vec::new();

        for op in ops {
            let id = TransitionId::new();
            let (kind, mirror_kind) = match op {
                RandomOp::Apply(v) => (
                    LogEntryKind::Apply(Box::new(Add(v)) as I64Transition),
                    MirrorKind::Apply(v),
                ),
                RandomOp::Undo(idx) => {
                    if all_ids.is_empty() {
                        continue;
                    }
                    let target = all_ids[idx % all_ids.len()];
                    (LogEntryKind::Undo(target), MirrorKind::Undo(target))
                }
                // may target a non-`Apply` or an undone entry, then `apply` rejects it
                RandomOp::Replace(idx, v) => {
                    if all_ids.is_empty() {
                        continue;
                    }
                    let target = all_ids[idx % all_ids.len()];
                    (
                        LogEntryKind::Replace {
                            target,
                            with: Box::new(Add(v)) as I64Transition,
                        },
                        MirrorKind::Replace(target, v),
                    )
                }
            };

            if sm.apply(LogEntry::new(id, kind)).is_ok() {
                mirror.push((id, mirror_kind));
                all_ids.push(id);
            }
            for (index, id) in all_ids.iter().enumerate() {
                let entry = sm.entry(*id).map(|entry| entry as *const _);
                proptest::prop_assert_eq!(entry, Some(&sm.log()[index] as *const _));
            }

            let rebuilt_log: Vec<LogEntry<I64Transition>> = mirror
                .iter()
                .map(|(id, kind)| {
                    let kind = match kind {
                        MirrorKind::Apply(v) => {
                            LogEntryKind::Apply(Box::new(Add(*v)) as I64Transition)
                        }
                        MirrorKind::Undo(target) => LogEntryKind::Undo(*target),
                        MirrorKind::Replace(target, v) => LogEntryKind::Replace {
                            target: *target,
                            with: Box::new(Add(*v)) as I64Transition,
                        },
                    };
                    LogEntry::new(*id, kind)
                })
                .collect();
            let rebuilt = StateMachine::<i64, I64Transition>::build(rebuilt_log).unwrap();

            proptest::prop_assert_eq!(*sm.current(), *rebuilt.current());
            // `apply` updates the effects in place for an `Apply` or `Replace`, `build`
            // computes them all at once
            proptest::prop_assert_eq!(&sm.effects, &rebuilt.effects);
        }
    }

    // The same invariant, but checked against a model that shares no code
    // with `replay` and over a transition that doesn't commute. Comparing
    // `apply` against `build` (above) cannot see a replay that runs the
    // active set in the wrong order, since both go through `replay`.
    #[test]
    fn current_matches_an_independent_model_of_the_active_set(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::char::range('a', 'z').prop_map(PushOp::Push),
                proptest::prelude::any::<usize>().prop_map(PushOp::Undo),
                (proptest::prelude::any::<usize>(), proptest::char::range('A', 'Z'))
                    .prop_map(|(idx, c)| PushOp::Replace(idx, c)),
            ],
            0..50,
        )
    ) {
        let mut sm = StateMachine::<String, StringTransition>::default();
        let mut mirror: Vec<(TransitionId, PushKind)> = Vec::new();

        for op in ops {
            let id = TransitionId::new();
            let kind = match op {
                PushOp::Push(c) => PushKind::Push(c),
                PushOp::Undo(idx) => {
                    if mirror.is_empty() {
                        continue;
                    }
                    PushKind::Undo(mirror[idx % mirror.len()].0)
                }
                PushOp::Replace(idx, c) => {
                    if mirror.is_empty() {
                        continue;
                    }
                    PushKind::Replace(mirror[idx % mirror.len()].0, c)
                }
            };
            let entry = LogEntry::new(id, match &kind {
                    PushKind::Push(c) => {
                        LogEntryKind::Apply(Box::new(Push(*c)) as StringTransition)
                    }
                    PushKind::Undo(target) => LogEntryKind::Undo(*target),
                    PushKind::Replace(target, c) => LogEntryKind::Replace {
                        target: *target,
                        with: Box::new(Push(*c)) as StringTransition,
                    },
                });

            // `Push` is applicable to any state and never fails, so only
            // a replace of an entry that isn't an active push can be
            // rejected.
            let rejected = match &kind {
                PushKind::Replace(target, _) => {
                    let index = mirror.iter().position(|(id, _)| id == target).unwrap();
                    !matches!(mirror[index].1, PushKind::Push(_))
                        || !expected_active(&mirror)[index]
                }
                PushKind::Push(_) | PushKind::Undo(_) => false,
            };
            proptest::prop_assert_eq!(sm.apply(entry).is_err(), rejected);
            if rejected {
                continue;
            }
            mirror.push((id, kind));

            proptest::prop_assert_eq!(sm.current(), &expected_state(&mirror));
            let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
            let expected: Vec<_> = mirror
                .iter()
                .zip(expected_active(&mirror))
                .rev()
                .filter_map(|((id, _), active)| active.then_some(*id))
                .collect();
            proptest::prop_assert_eq!(in_effect, expected);
            proptest::prop_assert_eq!(&sm.effects, &expected_effects(&mirror));

            for ((id, kind), active) in mirror.iter().zip(expected_active(&mirror)) {
                let mut effective = String::new();
                if let Some(transition) = sm.effective_transition(*id) {
                    transition.apply(&mut effective).unwrap();
                }
                let expected = match kind {
                    PushKind::Push(c) if active => expected_char(&mirror, *id, *c).to_string(),
                    PushKind::Push(_) | PushKind::Undo(_) | PushKind::Replace(..) => {
                        String::new()
                    }
                };
                proptest::prop_assert_eq!(effective, expected);
            }
        }
    }
}

/// What a random `apply` call in the property test above does, over
/// non-commuting `Push` transitions: either append a character, or undo or
/// replace a previously-appended entry (picked by index into `mirror`,
/// wrapped so every index is valid).
#[derive(Clone, Debug)]
enum PushOp {
    Push(char),
    Undo(usize),
    Replace(usize, char),
}

enum PushKind {
    Push(char),
    Undo(TransitionId),
    Replace(TransitionId, char),
}

/// Model of what the state machine's `current()` should be for a log of
/// `Push`/`Undo`/`Replace` entries, written independently of
/// `StateMachine::replay`: an entry is active unless some still-active
/// `Undo` targets it, and the surviving pushes apply in log order, each
/// with the character of its latest active replace, if any.
fn expected_state(entries: &[(TransitionId, PushKind)]) -> String {
    entries
        .iter()
        .zip(expected_active(entries))
        .filter_map(|((id, kind), active)| match kind {
            PushKind::Push(c) if active => Some(expected_char(entries, *id, *c)),
            PushKind::Push(_) | PushKind::Undo(_) | PushKind::Replace(..) => None,
        })
        .collect()
}

/// The character the push with the id `id` and the character `c` pushes
/// in `entries`: the one of its latest active replace, or `c` if there is
/// none.
fn expected_char(entries: &[(TransitionId, PushKind)], id: TransitionId, c: char) -> char {
    entries
        .iter()
        .zip(expected_active(entries))
        .rev()
        .find_map(|((_, kind), active)| match kind {
            PushKind::Replace(target, replacement) if active && *target == id => Some(*replacement),
            _ => None,
        })
        .unwrap_or(c)
}

/// The effect of each of `entries`, written independently of how `StateMachine` keeps them up
/// to date: an entry is undone if an active `Undo` targets it, a push is replaced by its latest
/// active replace, which is the active replace of its target even if that is undone.
fn expected_effects(entries: &[(TransitionId, PushKind)]) -> Vec<EntryEffect> {
    let active = expected_active(entries);
    let latest_replace = |target: TransitionId| {
        (0..entries.len())
            .rev()
            .find(|&i| active[i] && matches!(entries[i].1, PushKind::Replace(t, _) if t == target))
    };
    (0..entries.len())
        .map(|index| {
            if !active[index] {
                return EntryEffect::Undone;
            }
            match entries[index].1 {
                PushKind::Push(_) => latest_replace(entries[index].0)
                    .map_or(EntryEffect::Active, |by| EntryEffect::Replaced { by }),
                PushKind::Undo(_) => EntryEffect::Active,
                PushKind::Replace(target, _) if latest_replace(target) == Some(index) => {
                    EntryEffect::ActiveReplace
                }
                PushKind::Replace(..) => EntryEffect::InactiveReplace,
            }
        })
        .collect()
}

/// Whether each of `entries` is active, i.e. not targeted by a still-active `Undo`, written
/// independently of `StateMachine`'s effects.
fn expected_active(entries: &[(TransitionId, PushKind)]) -> Vec<bool> {
    let mut active = vec![false; entries.len()];
    // Back to front: an `Undo` can only target an earlier entry, so
    // whether entry `index` survives depends only on already-decided ones.
    for index in (0..entries.len()).rev() {
        let target_id = entries[index].0;
        active[index] = !entries
            .iter()
            .enumerate()
            .skip(index + 1)
            .any(|(i, (_, kind))| {
                active[i] && matches!(kind, PushKind::Undo(target) if *target == target_id)
            });
    }
    active
}

#[test]
fn whether_an_entry_is_in_effect_depends_on_the_undos_in_effect_after_it() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let a = apply_get_id(&mut sm, push_entry('a'));
    let undo = apply_get_id(
        &mut sm,
        LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a)),
    );
    let redo = apply_get_id(
        &mut sm,
        LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo)),
    );

    assert_eq!(sm.is_in_effect(a), Some(true));
    assert_eq!(sm.is_in_effect(undo), Some(false));
    assert_eq!(sm.is_in_effect(redo), Some(true));
    assert_eq!(sm.is_in_effect(TransitionId::new()), None);
}

#[cfg(feature = "serde")]
mod serde_support {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::TransitionError;

    // A transition type that is itself (de)serializable, which `Box<dyn
    // Transition<i64>>` can't be. `Mul` makes the log non-commutative, so
    // a round trip that lost the log's order would be visible.
    #[derive(Debug, Serialize, Deserialize)]
    enum Arith {
        Add(i64),
        Mul(i64),
    }

    impl Transition<i64> for Arith {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            match self {
                Arith::Add(v) => *state += v,
                Arith::Mul(v) => *state *= v,
            }
            Ok(())
        }
    }

    #[test]
    fn a_log_round_trips_through_json() {
        let add = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Arith::Add(5)));
        let mul = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Arith::Mul(3)));
        let log = vec![add, mul];

        let json = serde_json::to_string(&log).unwrap();
        let restored: Vec<LogEntry<Arith>> = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].id(), log[0].id());
        assert_eq!(restored[1].id(), log[1].id());
        // (5 + 0) * 3, not 0 * 3 + 5: the entries keep their order.
        assert_eq!(*StateMachine::build(restored).unwrap().current(), 15);
    }

    #[test]
    fn a_state_machine_round_trips_through_json_with_its_undos() {
        let mut sm = StateMachine::<i64, Arith>::new();
        sm.apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Apply(Arith::Add(5)),
        ))
        .unwrap();
        let doubling_id = TransitionId::new();
        sm.apply(LogEntry::new(
            doubling_id,
            LogEntryKind::Apply(Arith::Mul(2)),
        ))
        .unwrap();
        let undo_id = TransitionId::new();
        sm.apply(LogEntry::new(undo_id, LogEntryKind::Undo(doubling_id)))
            .unwrap();
        assert_eq!(*sm.current(), 5);

        let json = serde_json::to_string(&sm).unwrap();
        let mut restored: StateMachine<i64, Arith> = serde_json::from_str(&json).unwrap();

        // `current` is recomputed from the restored log, undone entry included.
        assert_eq!(*restored.current(), 5);
        assert_eq!(restored.log().len(), 3);

        // The undo entry survived as an entry, so it is still undoable.
        restored
            .apply(LogEntry::new(
                TransitionId::new(),
                LogEntryKind::Undo(undo_id),
            ))
            .unwrap();
        assert_eq!(*restored.current(), 10);
    }

    #[test]
    fn deserializing_a_state_machine_rejects_a_log_that_build_rejects() {
        let unknown_id = TransitionId::new();
        let log: Vec<LogEntry<Arith>> = vec![LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Undo(unknown_id),
        )];
        let json = serde_json::to_string(&log).unwrap();

        let err = serde_json::from_str::<StateMachine<i64, Arith>>(&json).unwrap_err();

        assert!(
            err.to_string().contains("targets unknown transition"),
            "unexpected error: {err}"
        );
    }
}

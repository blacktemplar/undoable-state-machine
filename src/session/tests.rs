use super::*;
use crate::test_util::{
    Add, I64Transition, RequireAtLeast, StringTransition, apply_entry, apply_get_id, push_entry,
    replace_push_entry,
};

/// Pushes `c` as an action of `history`, and returns the id of its entry.
fn push_in(
    history: &mut SessionHistory,
    sm: &mut StateMachine<String, StringTransition>,
    c: char,
) -> TransitionId {
    let entry = push_entry(c);
    let id = entry.id();
    history.apply(sm, entry).unwrap();
    id
}

#[test]
fn a_session_undoes_and_redoes_its_actions_in_order() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    for c in ['a', 'b', 'c'] {
        push_in(&mut history, &mut sm, c);
    }

    history.undo_last(&mut sm).unwrap();
    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "a");
    history.redo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "ab");
    history.undo_last(&mut sm).unwrap();
    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "");

    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::NothingToUndo)
    ));
    for expected in ["a", "ab", "abc"] {
        history.redo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), expected);
    }
    assert!(matches!(
        history.redo_last(&mut sm),
        Err(RedoLastError::NothingToRedo)
    ));
}

#[test]
fn apply_new_is_an_action_of_the_session() {
    let mut sm = StateMachine::<i64, I64Transition>::new();
    let mut history = SessionHistory::new();
    let id = history.apply_new(&mut sm, Box::new(Add(5))).unwrap();

    assert_eq!(history.next_undo(&sm), Some(id));
    history.undo_last(&mut sm).unwrap();
    assert_eq!(*sm.current(), 0);
}

#[test]
fn apply_new_that_is_rejected_changes_neither_the_log_nor_the_history() {
    let mut sm = StateMachine::<i64, I64Transition>::new();
    let mut history = SessionHistory::new();

    let result = history.apply_new(&mut sm, Box::new(RequireAtLeast(1)));

    assert!(result.is_err());
    assert!(sm.log().is_empty());
    assert_eq!(history.next_undo(&sm), None);
}

#[test]
fn a_new_action_leaves_nothing_to_redo() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    push_in(&mut history, &mut sm, 'a');
    history.undo_last(&mut sm).unwrap();

    push_in(&mut history, &mut sm, 'b');

    assert!(matches!(
        history.redo_last(&mut sm),
        Err(RedoLastError::NothingToRedo)
    ));
    assert_eq!(sm.current(), "b");
}

#[test]
fn a_session_undoes_its_own_actions_rather_than_those_of_another_session() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    push_in(&mut history, &mut sm, 'a');
    sm.apply(push_entry('b')).unwrap();

    history.undo_last(&mut sm).unwrap();

    assert_eq!(sm.current(), "b");
    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::NothingToUndo)
    ));
}

/// An undo targeting an entry of the session's choice is an action like any other, so undoing
/// it brings back what it undid.
#[test]
fn undoing_a_targeted_undo_brings_back_what_it_undid() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    let a = push_in(&mut history, &mut sm, 'a');
    push_in(&mut history, &mut sm, 'b');
    history
        .apply(
            &mut sm,
            LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a)),
        )
        .unwrap();
    assert_eq!(sm.current(), "b");

    history.undo_last(&mut sm).unwrap();

    assert_eq!(sm.current(), "ab");
    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "a");
}

#[test]
fn undoing_a_replacement_brings_back_the_replaced_transition() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    let a = push_in(&mut history, &mut sm, 'a');
    push_in(&mut history, &mut sm, 'b');
    history.apply(&mut sm, replace_push_entry(a, 'x')).unwrap();
    assert_eq!(sm.current(), "xb");

    history.undo_last(&mut sm).unwrap();

    assert_eq!(sm.current(), "ab");
}

/// An action another session undid is not passed over, which would undo an older action while
/// a newer one could still come back, e.g. if the other session undoes its undo.
#[test]
fn an_action_another_session_undid_is_not_offered_until_it_is_back() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    push_in(&mut history, &mut sm, 'a');
    let b = push_in(&mut history, &mut sm, 'b');
    let elsewhere = apply_get_id(
        &mut sm,
        LogEntry::new(TransitionId::new(), LogEntryKind::Undo(b)),
    );

    assert_eq!(history.next_undo(&sm), None);

    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Undo(elsewhere),
    ))
    .unwrap();
    assert_eq!(history.next_undo(&sm), Some(b));
    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "a");
}

#[test]
fn undoing_an_action_another_session_undid_leaves_nothing_to_undo_but_keeps_what_to_redo() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    push_in(&mut history, &mut sm, 'a');
    let b = push_in(&mut history, &mut sm, 'b');
    let c = push_in(&mut history, &mut sm, 'c');
    history.undo_last(&mut sm).unwrap();
    sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(b)))
        .unwrap();
    let log_len = sm.log().len();

    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::UndoneElsewhere)
    ));

    assert_eq!(sm.log().len(), log_len);
    assert_eq!(sm.current(), "a");
    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::NothingToUndo)
    ));
    assert_eq!(history.next_redo(&sm), Some(c));
    history.redo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "ac");
}

#[test]
fn redoing_an_action_another_session_redid_leaves_nothing_to_redo_but_keeps_what_to_undo() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    let a = push_in(&mut history, &mut sm, 'a');
    push_in(&mut history, &mut sm, 'b');
    push_in(&mut history, &mut sm, 'c');
    history.undo_last(&mut sm).unwrap();
    let undo = history.undo_last(&mut sm).unwrap();
    sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo)))
        .unwrap();
    let log_len = sm.log().len();
    assert_eq!(history.next_redo(&sm), None);

    assert!(matches!(
        history.redo_last(&mut sm),
        Err(RedoLastError::RedoneElsewhere)
    ));

    assert_eq!(sm.log().len(), log_len);
    assert_eq!(sm.current(), "ab");
    assert!(matches!(
        history.redo_last(&mut sm),
        Err(RedoLastError::NothingToRedo)
    ));
    assert_eq!(history.next_undo(&sm), Some(a));
    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "b");
}

/// Entries of other sessions are no actions of this one, so they leave what it undid to redo.
#[test]
fn an_entry_of_another_session_leaves_what_to_redo() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    let a = push_in(&mut history, &mut sm, 'a');
    history.undo_last(&mut sm).unwrap();

    sm.apply(push_entry('b')).unwrap();

    assert_eq!(history.next_redo(&sm), Some(a));
    history.redo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "ab");
}

/// An action the session undid itself, by an undo targeting it, lies below that undo, so it is
/// in effect again by the time it is the latest action.
#[test]
fn an_action_the_session_undid_itself_is_undone_after_that_undo() {
    let mut sm = StateMachine::<String, StringTransition>::new();
    let mut history = SessionHistory::new();
    let a = push_in(&mut history, &mut sm, 'a');
    history
        .apply(
            &mut sm,
            LogEntry::new(TransitionId::new(), LogEntryKind::Undo(a)),
        )
        .unwrap();
    assert_eq!(sm.current(), "");

    history.undo_last(&mut sm).unwrap();
    assert_eq!(sm.current(), "a");
    history.undo_last(&mut sm).unwrap();

    assert_eq!(sm.current(), "");
    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::NothingToUndo)
    ));
}

#[test]
fn an_undo_an_entry_of_another_session_depends_on_is_rejected_and_leaves_nothing_to_undo() {
    let mut sm = StateMachine::<i64, I64Transition>::new();
    let mut history = SessionHistory::new();
    history.apply(&mut sm, apply_entry(5)).unwrap();
    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Apply(Box::new(RequireAtLeast(5))),
    ))
    .unwrap();
    let log_len = sm.log().len();

    let result = history.undo_last(&mut sm);

    assert!(matches!(result, Err(UndoLastError::Rejected(_))));
    assert_eq!(sm.log().len(), log_len);
    assert_eq!(*sm.current(), 5);
    assert!(matches!(
        history.undo_last(&mut sm),
        Err(UndoLastError::NothingToUndo)
    ));
}

#[test]
fn a_redo_that_no_longer_applies_is_rejected_and_leaves_nothing_to_redo() {
    let mut sm = StateMachine::<i64, I64Transition>::new();
    let mut history = SessionHistory::new();
    let first = apply_entry(1);
    let first_id = first.id();
    history.apply(&mut sm, first).unwrap();
    let elsewhere = apply_get_id(&mut sm, apply_entry(5));
    history
        .apply(
            &mut sm,
            LogEntry::new(
                TransitionId::new(),
                LogEntryKind::Apply(Box::new(RequireAtLeast(6))),
            ),
        )
        .unwrap();
    history.undo_last(&mut sm).unwrap();
    sm.apply(LogEntry::new(
        TransitionId::new(),
        LogEntryKind::Undo(elsewhere),
    ))
    .unwrap();
    let log_len = sm.log().len();

    let result = history.redo_last(&mut sm);

    assert!(matches!(result, Err(RedoLastError::Rejected(_))));
    assert_eq!(sm.log().len(), log_len);
    assert_eq!(*sm.current(), 1);
    assert!(matches!(
        history.redo_last(&mut sm),
        Err(RedoLastError::NothingToRedo)
    ));
    assert_eq!(history.next_undo(&sm), Some(first_id));
}

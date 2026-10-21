//! What each entry of a [`StateMachine`]'s log does given the entries after it, which the state
//! machine caches.

use std::collections::{HashMap, hash_map::Entry};

#[cfg(doc)]
use super::{StateMachine, check_target};
use crate::{LogEntry, LogEntryKind, TransitionId};

/// What an entry of a [`StateMachine`]'s log does, given the entries after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EntryEffect {
    /// An `Undo` in effect, or an `Apply` in effect that runs its own transition.
    Active,
    /// An `Apply` in effect that runs the transition of the `Replace` at the index `by` of the log
    /// in place of its own: the latest `Replace` of it in effect.
    Replaced { by: usize },
    /// The latest `Replace` in effect of its target. Its transition runs in place of the target's,
    /// or, if the target is undone, will once the target is redone.
    ActiveReplace,
    /// A `Replace` in effect that a later `Replace` in effect of the same target shadows.
    InactiveReplace,
    /// An entry cancelled by a later `Undo` in effect.
    Undone,
}

impl EntryEffect {
    pub(super) fn is_in_effect(self) -> bool {
        self != EntryEffect::Undone
    }
}

/// Computes the effect of every entry of `log`, where `index_of` is the index of every id in it.
///
/// Expects the target of every `Undo` and `Replace` to be an earlier entry (see [`check_target`]).
pub(super) fn compute_effects<T>(
    log: &[LogEntry<T>],
    index_of: &HashMap<TransitionId, usize>,
) -> Vec<EntryEffect> {
    // only a later entry can cancel or replace an entry, so by the time the backward walk reaches
    // an entry it has seen every undo in effect of it and every replacement in effect of it; until
    // then, `Active` is a placeholder
    let mut effects = vec![EntryEffect::Active; log.len()];
    // per replaced `Apply`, the index of its latest `Replace` in effect, i.e. the first one the
    // walk met
    let mut active_replacements = HashMap::new();
    for (index, entry) in log.iter().enumerate().rev() {
        if effects[index] == EntryEffect::Undone {
            continue;
        }
        effects[index] = match *entry.kind() {
            LogEntryKind::Apply(_) => active_replacements
                .get(&index)
                .map_or(EntryEffect::Active, |&by| EntryEffect::Replaced { by }),
            LogEntryKind::Undo(target) => {
                effects[index_of[&target]] = EntryEffect::Undone;
                EntryEffect::Active
            }
            LogEntryKind::Replace { target, .. } => {
                match active_replacements.entry(index_of[&target]) {
                    Entry::Vacant(vacant) => {
                        vacant.insert(index);
                        EntryEffect::ActiveReplace
                    }
                    Entry::Occupied(_) => EntryEffect::InactiveReplace,
                }
            }
        };
    }
    effects
}

/// The transition of the [`LogEntryKind::Replace`] at the index `index` of `log`.
fn replacing_transition<T>(log: &[LogEntry<T>], index: usize) -> &T {
    match log[index].kind() {
        LogEntryKind::Replace { with, .. } => with,
        LogEntryKind::Apply(_) | LogEntryKind::Undo(_) => {
            unreachable!("only a `Replace` replaces an `Apply`")
        }
    }
}

/// The transition that runs at the index `index` of `log`, given the `effects` of its entries,
/// with the id of the entry it comes from: the transition of an `Apply` in effect, or the one of
/// its active replacement if it has one. `None` for any other entry.
pub(super) fn transition_running_at<'a, T>(
    log: &'a [LogEntry<T>],
    effects: &[EntryEffect],
    index: usize,
) -> Option<(TransitionId, &'a T)> {
    match (effects[index], log[index].kind()) {
        (EntryEffect::Active, LogEntryKind::Apply(transition)) => {
            Some((log[index].id(), transition))
        }
        (EntryEffect::Replaced { by }, _) => Some((log[by].id(), replacing_transition(log, by))),
        _ => None,
    }
}

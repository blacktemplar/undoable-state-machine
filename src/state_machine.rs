//! The [`StateMachine`], which keeps an append-only log and the state that replaying it results
//! in.

mod effects;
#[cfg(feature = "serde")]
mod serde_impls;
#[cfg(test)]
mod tests;

use std::{collections::HashMap, fmt};

use self::effects::{EntryEffect, compute_effects, transition_running_at};
use crate::{LogEntry, LogEntryKind, StateMachineError, Transition, TransitionId};

pub struct StateMachine<S, T> {
    log: Vec<LogEntry<T>>,
    /// The index in `log` of every id currently in it, kept in sync with it so id lookups
    /// (duplicate rejection, [`StateMachine::entry`]) are O(1) instead of a linear scan over a
    /// potentially long log.
    index_of: HashMap<TransitionId, usize>,
    /// The effect of every entry of `log`, at the same index, kept in sync with it so that whether
    /// an entry is in effect, and which transition runs in place of an `Apply`, is known without a
    /// walk over the log.
    effects: Vec<EntryEffect>,
    current: S,
}

impl<S: fmt::Debug, T: fmt::Debug> fmt::Debug for StateMachine<S, T> {
    // Omits the internal caches `index_of` and `effects`, which are derived from `log`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateMachine")
            .field("log", &self.log)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl<S: Default, T> Default for StateMachine<S, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, T> StateMachine<S, T> {
    /// Returns the current state: the result of replaying every log entry that is still active
    /// (i.e. not cancelled by a later, still-active [`LogEntryKind::Undo`]).
    pub fn current(&self) -> &S {
        &self.current
    }

    /// Returns the full append-only log, including entries that have since been undone.
    pub fn log(&self) -> &[LogEntry<T>] {
        &self.log
    }

    /// Returns the entry of the log with the id `id`, whether or not it is in effect, or `None` if
    /// the log has no such entry. Takes constant time.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let mut sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();
    /// let (add_5, undo_add_5) = (TransitionId::new(), TransitionId::new());
    /// sm.apply(LogEntry::new(add_5, LogEntryKind::Apply(Box::new(Add(5)))))?;
    /// sm.apply(LogEntry::new(undo_add_5, LogEntryKind::Undo(add_5)))?;
    ///
    /// // the undone entry is still found
    /// assert!(matches!(
    ///     sm.entry(add_5).map(LogEntry::kind),
    ///     Some(LogEntryKind::Apply(_))
    /// ));
    /// assert!(sm.entry(TransitionId::new()).is_none());
    /// # Ok::<(), StateMachineError>(())
    /// ```
    pub fn entry(&self, id: TransitionId) -> Option<&LogEntry<T>> {
        self.index_of.get(&id).map(|&index| &self.log[index])
    }

    /// Returns the transition that runs in place of the [`LogEntryKind::Apply`] with the id `id`:
    /// the one of the latest [`LogEntryKind::Replace`] of it that is in effect, or the `Apply`'s
    /// own transition if there is none. Returns `None` if the log has no `Apply` with this id, or
    /// if that `Apply` is undone, since then nothing runs in its place.
    ///
    /// Takes constant time.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// #[derive(Debug, PartialEq)]
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let mut sm = StateMachine::<i64, Add>::new();
    /// let (add, replace) = (TransitionId::new(), TransitionId::new());
    /// sm.apply(LogEntry::new(add, LogEntryKind::Apply(Add(5))))?;
    /// assert_eq!(sm.effective_transition(add), Some(&Add(5)));
    ///
    /// sm.apply(LogEntry::new(
    ///     replace,
    ///     LogEntryKind::Replace {
    ///         target: add,
    ///         with: Add(7),
    ///     },
    /// ))?;
    /// assert_eq!(sm.effective_transition(add), Some(&Add(7)));
    /// // only an `Apply` has an effective transition
    /// assert_eq!(sm.effective_transition(replace), None);
    ///
    /// // and only while it is in effect
    /// sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(add)))?;
    /// assert_eq!(sm.effective_transition(add), None);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    pub fn effective_transition(&self, id: TransitionId) -> Option<&T> {
        let &index = self.index_of.get(&id)?;
        transition_running_at(&self.log, &self.effects, index).map(|(_, transition)| transition)
    }

    /// Returns the log entries that are in effect, i.e. not cancelled by a later
    /// [`LogEntryKind::Undo`] that is in effect itself, from the newest to the oldest.
    ///
    /// Replacing doesn't cancel anything: a [`LogEntryKind::Replace`] that is in effect leaves its
    /// target and earlier replacements of it in effect, even though only its own transition runs.
    /// A `Replace` of an undone `Apply` is in effect as well, although it has no effect until the
    /// `Apply` is redone.
    ///
    /// The iterator is lazy: it only walks the log as far back as it is advanced, so finding the
    /// latest entry in effect that matches some condition costs only the part of the log that
    /// lies after it.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let (add_5, add_7, undo_add_7) = (
    ///     TransitionId::new(),
    ///     TransitionId::new(),
    ///     TransitionId::new(),
    /// );
    /// let log: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    ///     LogEntry::new(add_5, LogEntryKind::Apply(Box::new(Add(5)))),
    ///     LogEntry::new(add_7, LogEntryKind::Apply(Box::new(Add(7)))),
    ///     LogEntry::new(undo_add_7, LogEntryKind::Undo(add_7)),
    /// ];
    /// let sm = StateMachine::build(log)?;
    /// let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
    /// assert_eq!(in_effect, [undo_add_7, add_5]);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    pub fn rev_in_effect(&self) -> impl Iterator<Item = &LogEntry<T>> {
        self.log
            .iter()
            .zip(&self.effects)
            .rev()
            .filter_map(|(entry, effect)| effect.is_in_effect().then_some(entry))
    }

    /// Returns whether the entry of the log with the id `id` is in effect, i.e. not cancelled by
    /// a later [`LogEntryKind::Undo`] that is in effect itself, or `None` if the log has no such
    /// entry.
    ///
    /// Takes constant time.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let mut sm = StateMachine::<i64, Add>::new();
    /// let (add, undo) = (TransitionId::new(), TransitionId::new());
    /// sm.apply(LogEntry::new(add, LogEntryKind::Apply(Add(5))))?;
    /// assert_eq!(sm.is_in_effect(add), Some(true));
    ///
    /// sm.apply(LogEntry::new(undo, LogEntryKind::Undo(add)))?;
    /// assert_eq!(sm.is_in_effect(add), Some(false));
    /// assert_eq!(sm.is_in_effect(undo), Some(true));
    /// assert_eq!(sm.is_in_effect(TransitionId::new()), None);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    pub fn is_in_effect(&self, id: TransitionId) -> Option<bool> {
        self.index_of
            .get(&id)
            .map(|&index| self.effects[index].is_in_effect())
    }

    /// Updates `effects` for the entry just pushed to `log`. Nothing after the entry can cancel
    /// it, so it is in effect.
    ///
    /// Expects the target of an `Undo` to be an earlier entry (see [`check_target`]), and the
    /// target of a `Replace` to be an earlier `Apply` in effect, as [`StateMachine::apply`]
    /// ensures.
    fn push_effect(&mut self) {
        let index = self.effects.len();
        match *self.log[index].kind() {
            LogEntryKind::Apply(_) => self.effects.push(EntryEffect::Active),
            // an undo can cancel or bring back any number of entries, which in turn can change the
            // active replacements, so it is simplest to start over
            LogEntryKind::Undo(_) => self.effects = compute_effects(&self.log, &self.index_of),
            LogEntryKind::Replace { target, .. } => {
                let target = self.index_of[&target];
                match self.effects[target] {
                    EntryEffect::Active => {}
                    EntryEffect::Replaced { by } => self.effects[by] = EntryEffect::InactiveReplace,
                    EntryEffect::ActiveReplace
                    | EntryEffect::InactiveReplace
                    | EntryEffect::Undone => {
                        unreachable!("a pushed `Replace` targets an `Apply` in effect")
                    }
                }
                self.effects[target] = EntryEffect::Replaced { by: index };
                self.effects.push(EntryEffect::ActiveReplace);
            }
        }
    }
}

/// Checks that `target`, the target of a [`LogEntryKind::Undo`] or [`LogEntryKind::Replace`] at
/// the index `entry_index` of the log, is an entry before it. Returns `target`'s index on success.
fn check_target(
    index_of: &HashMap<TransitionId, usize>,
    target: TransitionId,
    entry_index: usize,
) -> Result<usize, StateMachineError> {
    match index_of.get(&target) {
        Some(&index) if index < entry_index => Ok(index),
        _ => Err(StateMachineError::UnknownTarget(target)),
    }
}

/// Checks that `target`, the target of a [`LogEntryKind::Replace`] at index `replace_index` of
/// `log`, is an [`LogEntryKind::Apply`] before it. Returns `target`'s index on success.
fn check_replace_target<T>(
    log: &[LogEntry<T>],
    index_of: &HashMap<TransitionId, usize>,
    target: TransitionId,
    replace_index: usize,
) -> Result<usize, StateMachineError> {
    let index = check_target(index_of, target, replace_index)?;
    match log[index].kind() {
        LogEntryKind::Apply(_) => Ok(index),
        LogEntryKind::Undo(_) | LogEntryKind::Replace { .. } => {
            Err(StateMachineError::ReplaceTargetNotApply(target))
        }
    }
}

impl<S: Default, T> StateMachine<S, T> {
    /// Creates an empty state machine with `S::default()` as the current state and no log
    /// entries.
    ///
    /// Use [`Self::build`] to reconstruct a state machine from an existing log instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{StateMachine, Transition};
    ///
    /// let sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();
    /// assert_eq!(*sm.current(), 0);
    /// assert!(sm.log().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            log: Vec::default(),
            index_of: HashMap::default(),
            effects: Vec::default(),
            current: Default::default(),
        }
    }
}

impl<S: Default, T: Transition<S>> StateMachine<S, T> {
    /// Reconstructs a state machine from a previously recorded log, e.g. one persisted elsewhere
    /// or merged from multiple sources.
    ///
    /// Equivalent to starting from [`Self::new`] and calling [`Self::apply`] with each entry in
    /// order, but computes `current` in a single backward-then-forward pass instead of replaying
    /// the growing log again after every entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let add_7_transition_id = TransitionId::new();
    /// let log: Vec<LogEntry<Box<dyn Transition<i64>>>> = vec![
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(5)))),
    ///     LogEntry::new(add_7_transition_id, LogEntryKind::Apply(Box::new(Add(7)))),
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Box::new(Add(-3)))),
    ///     LogEntry::new(TransitionId::new(), LogEntryKind::Undo(add_7_transition_id)),
    /// ];
    /// let sm = StateMachine::build(log)?;
    /// assert_eq!(*sm.current(), 2);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::apply`]'s `Undo` and `Replace` cases:
    /// an unknown undo or replace target, a replace target that is not an `Apply`, or an active
    /// transition that fails to apply. Also returns [`StateMachineError::DuplicateId`] if two
    /// entries in `log` share the same id.
    ///
    /// Unlike [`Self::apply`], accepts a `Replace` whose target is undone at that point of the log,
    /// e.g. in a log merged from multiple sources. Such a `Replace` has no effect until its target
    /// is redone.
    pub fn build(log: Vec<LogEntry<T>>) -> Result<Self, StateMachineError> {
        let mut index_of = HashMap::with_capacity(log.len());
        for (index, entry) in log.iter().enumerate() {
            if index_of.insert(entry.id(), index).is_some() {
                return Err(StateMachineError::DuplicateId(entry.id()));
            }
        }
        for (index, entry) in log.iter().enumerate() {
            match *entry.kind() {
                LogEntryKind::Apply(_) => {}
                LogEntryKind::Undo(target) => {
                    check_target(&index_of, target, index)?;
                }
                LogEntryKind::Replace { target, .. } => {
                    check_replace_target(&log, &index_of, target, index)?;
                }
            }
        }
        let effects = compute_effects(&log, &index_of);
        let current = Self::replay(&log, &effects)?;
        Ok(Self {
            log,
            index_of,
            effects,
            current,
        })
    }

    /// Applies `entry` and appends it to the log. On error, the log and [`Self::current`] are
    /// left unchanged.
    ///
    /// If the logs contain undos then calling this method is more efficient than applying all
    /// entries one by one with [`Self::apply`] since undone entries will never get applied in the
    /// first place. Note, however that this changes the semantics in case an undone entry would
    /// error. If you call [`Self::apply`] for each entry then it would error before applying the
    /// undo, while [`Self::apply`] fully ignores the undone entry and never applies it and never
    /// surfaces the error.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{
    ///     LogEntry, LogEntryKind, StateMachine, StateMachineError, Transition, TransitionError,
    ///     TransitionId,
    /// };
    ///
    /// struct Add(i64);
    ///
    /// impl Transition<i64> for Add {
    ///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
    ///         *state += self.0;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let mut sm = StateMachine::<i64, Box<dyn Transition<i64>>>::new();
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Apply(Box::new(Add(5))),
    /// ))?;
    /// assert_eq!(*sm.current(), 5);
    /// let add_7_transition_id = TransitionId::new();
    /// sm.apply(LogEntry::new(
    ///     add_7_transition_id,
    ///     LogEntryKind::Apply(Box::new(Add(7))),
    /// ))?;
    /// assert_eq!(*sm.current(), 12);
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Apply(Box::new(Add(-3))),
    /// ))?;
    /// assert_eq!(*sm.current(), 9);
    /// sm.apply(LogEntry::new(
    ///     TransitionId::new(),
    ///     LogEntryKind::Undo(add_7_transition_id),
    /// ))?;
    /// assert_eq!(*sm.current(), 2);
    /// # Ok::<(), StateMachineError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::Transition`] if the transition (for `Apply`), the replacing
    /// transition (for `Replace`) or some later active entry that depended on the target (for
    /// `Undo` and `Replace`) fails; [`StateMachineError::UnknownTarget`] if `Undo` or `Replace`
    /// targets an id not present earlier in the log. Returns
    /// [`StateMachineError::ReplaceTargetNotApply`] if `Replace` targets an entry that is not an
    /// `Apply`, and [`StateMachineError::ReplaceTargetNotInEffect`] if it targets an undone
    /// `Apply`. Returns [`StateMachineError::DuplicateId`] if `entry`'s id already appears in the
    /// log.
    ///
    /// # Panics
    ///
    /// Panics if healing the state after a failed `apply` (see [`Transition::apply`]'s
    /// determinism contract) itself fails to replay. This indicates a `Transition` impl that
    /// violates that contract, not a reachable failure under a correct one.
    pub fn apply(&mut self, entry: LogEntry<T>) -> Result<(), StateMachineError> {
        if self.index_of.contains_key(&entry.id()) {
            return Err(StateMachineError::DuplicateId(entry.id()));
        }
        match entry.kind() {
            LogEntryKind::Apply(transition) => match transition.check_applicable(&self.current) {
                Ok(()) => match transition.apply(&mut self.current) {
                    Ok(()) => {
                        self.index_of.insert(entry.id(), self.log.len());
                        self.log.push(entry);
                        self.push_effect();
                        Ok(())
                    }
                    Err(err) => {
                        // we need to replay the state
                        // the log and its effects are exactly what they were before this call,
                        // and that already replayed cleanly to produce the old `current`, so
                        // re-replaying it here cannot fail.
                        self.current = Self::replay(&self.log, &self.effects)
                            .expect("previously-active entries must still replay");
                        Err(StateMachineError::Transition(entry.id(), err))
                    }
                },
                Err(err) => Err(StateMachineError::Transition(entry.id(), err)),
            },
            LogEntryKind::Undo(target) => {
                check_target(&self.index_of, *target, self.log.len())?;
                self.push_and_replay(entry)
            }
            LogEntryKind::Replace { target, .. } => {
                check_replace_target(&self.log, &self.index_of, *target, self.log.len())?;
                if self.is_in_effect(*target) != Some(true) {
                    return Err(StateMachineError::ReplaceTargetNotInEffect(*target));
                }
                self.push_and_replay(entry)
            }
        }
    }

    /// Appends `entry`, an `Undo` or `Replace` whose target is checked already, and replays the log
    /// with it. On error, the log and [`Self::current`] are left unchanged.
    fn push_and_replay(&mut self, entry: LogEntry<T>) -> Result<(), StateMachineError> {
        let id = entry.id();
        // the duplicate check in `apply` makes this insert always succeed
        self.index_of.insert(id, self.log.len());
        self.log.push(entry);
        self.push_effect();
        match Self::replay(&self.log, &self.effects) {
            Ok(state) => {
                self.current = state;
                Ok(())
            }
            Err(err) => {
                self.log.pop();
                self.index_of.remove(&id);
                self.effects = compute_effects(&self.log, &self.index_of);
                Err(err)
            }
        }
    }

    /// Replays every `Apply` of `log` that is in effect according to `effects`, in log order, onto
    /// a fresh `S::default()`. A replaced `Apply` runs the transition of its active replacement
    /// instead of its own.
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::Transition`] if any active transition's
    /// [`Transition::check_applicable`] or [`Transition::apply`] fails. A failing replacement is
    /// reported with the id of its `Replace`.
    fn replay(log: &[LogEntry<T>], effects: &[EntryEffect]) -> Result<S, StateMachineError> {
        // transitions don't commute in general, they have to be applied in log order
        let active = (0..log.len()).filter_map(|index| transition_running_at(log, effects, index));

        let mut state = S::default();
        for (id, transition) in active {
            transition
                .check_applicable(&state)
                .map_err(|err| StateMachineError::Transition(id, err))?;
            transition
                .apply(&mut state)
                .map_err(|err| StateMachineError::Transition(id, err))?;
        }

        Ok(state)
    }
}

#![doc = include_str!("../README.md")]

use std::{
    collections::{HashMap, hash_map::Entry},
    error::Error,
    fmt,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Identifies a single [`LogEntry`] so a later entry can target it via [`LogEntryKind::Undo`].
///
/// Caller-assigned via [`TransitionId::new`]. Must be unique across a single [`StateMachine`]'s
/// log.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TransitionId(Uuid);

impl Default for TransitionId {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionId {
    /// Generates a new, randomly-assigned id.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::TransitionId;
    ///
    /// let id = TransitionId::new();
    /// assert_ne!(id, TransitionId::new());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Typically a log will contain many different transitions (using `Box<dyn Transition<S> + ...>`)
/// which can also return many different types of errors.
pub type TransitionError = Box<dyn Error + Send + Sync>;

/// A transition that mutates state, split into a pure pre-check and the mutation itself.
///
/// # Examples
///
/// ```
/// use undoable_state_machine::{Transition, TransitionError};
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
/// ```
pub trait Transition<S> {
    /// Checks whether the transition is applicable to `state`, without mutating it.
    ///
    /// # Errors
    /// Returns an error if the transition is not applicable to the state.
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError>;

    /// Applies the transition by mutating `state` in place.
    ///
    /// Only called after [`Transition::check_applicable`] returned `Ok(())`, and must be
    /// deterministic: the same input state must always produce the same mutation, since
    /// [`StateMachine::apply`] relies on this to recover by replaying the log if this method
    /// fails. On error, treat `state` as dirty and don't rely on its contents. Errors here
    /// should be rare - catch problems early in [`Transition::check_applicable`] instead.
    ///
    /// # Errors
    /// An error occurs during mutation that was not caught in [`Transition::check_applicable`].
    fn apply(&self, state: &mut S) -> Result<(), TransitionError>;
}

/// Useful when using `Box<dyn Transition<...> + ...>` as `T` in [`StateMachine`]
impl<S, T: ?Sized + Transition<S>> Transition<S> for Box<T> {
    fn check_applicable(&self, state: &S) -> Result<(), TransitionError> {
        (**self).check_applicable(state)
    }
    fn apply(&self, state: &mut S) -> Result<(), TransitionError> {
        (**self).apply(state)
    }
}

/// One entry in a [`StateMachine`]'s append-only log.
///
/// With the `serde` feature, an entry is (de)serializable whenever the transition type `T` is, so
/// a whole log (`Vec<LogEntry<T>>`), including its undo entries, can be persisted. Note that
/// `Box<dyn Transition<S>>` is not (de)serializable - use an enum of the transitions, or a crate
/// like [`typetag`](https://docs.rs/typetag), as `T` if you need to persist a log.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LogEntry<T> {
    id: TransitionId,
    kind: LogEntryKind<T>,
}

impl<T> LogEntry<T> {
    /// Creates a new log entry with the given id and kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use undoable_state_machine::{LogEntry, LogEntryKind, TransitionId};
    ///
    /// let entry = LogEntry::new(TransitionId::new(), LogEntryKind::Apply(5));
    /// match entry.kind() {
    ///     LogEntryKind::Apply(v) => assert_eq!(*v, 5),
    ///     LogEntryKind::Undo(_) | LogEntryKind::Replace { .. } => unreachable!(),
    /// }
    /// ```
    #[must_use]
    pub fn new(id: TransitionId, kind: LogEntryKind<T>) -> Self {
        Self { id, kind }
    }

    /// Uniquely identifies this entry so a later entry can target it via [`LogEntryKind::Undo`] or
    /// [`LogEntryKind::Replace`].
    pub fn id(&self) -> TransitionId {
        self.id
    }

    /// What this entry does: apply a transition, undo a previous entry, or replace a previous
    /// transition.
    pub fn kind(&self) -> &LogEntryKind<T> {
        &self.kind
    }
}

/// Represents applying a transition, undoing a previous entry, or replacing a previous transition.
///
/// Undo is just another log entry, which is what makes undo itself undoable. Undoing an `Undo`
/// reactivates its target, which is basically a redo.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum LogEntryKind<T> {
    /// Applies the given transition.
    Apply(T),
    /// Undoes the entry with the given id
    Undo(TransitionId),
    /// Runs `with` in place of the [`LogEntryKind::Apply`] with the id `target`, at that entry's
    /// position in the log rather than at the end.
    ///
    /// `target` is always the original `Apply`, never another `Replace`: replacing a transition
    /// again is another `Replace` of the same `Apply`. The latest `Replace` of an `Apply` that is
    /// in effect runs in its place, so undoing that `Replace` falls back to the previous one, or
    /// to the original transition if there is none. Undoing the targeted `Apply` itself undoes it
    /// together with all of its replacements.
    Replace {
        /// The id of the `Apply` entry whose transition is replaced.
        target: TransitionId,
        /// The transition that runs in place of the target's.
        with: T,
    },
}

/// An error from [`StateMachine::apply`] or [`StateMachine::build`].
#[derive(Debug)]
pub enum StateMachineError {
    /// A transition returned an error
    Transition(TransitionId, TransitionError),
    /// An `Undo` or `Replace` targeted an id not present anywhere earlier in the log.
    UnknownTarget(TransitionId),
    /// An entry's id already appears earlier in the log. [`TransitionId`] must be unique, reusing
    /// one would make `Undo` and `Replace` targets ambiguous.
    DuplicateId(TransitionId),
    /// A `Replace` targeted an entry with the given id that is not an `Apply`, e.g. another
    /// `Replace` instead of the `Apply` that one replaces.
    ReplaceTargetNotApply(TransitionId),
    /// [`StateMachine::apply`] got a `Replace` whose target, the entry with the given id, is not in
    /// effect, i.e. undone.
    ReplaceTargetNotInEffect(TransitionId),
}

impl fmt::Display for StateMachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateMachineError::Transition(id, err) => {
                write!(f, "transition {id:?} failed: {err}")
            }
            StateMachineError::UnknownTarget(id) => {
                write!(f, "log entry targets unknown transition {id:?}")
            }
            StateMachineError::DuplicateId(id) => {
                write!(f, "transition id {id:?} already appears in the log")
            }
            StateMachineError::ReplaceTargetNotApply(id) => {
                write!(f, "replace targets {id:?}, which is not an apply entry")
            }
            StateMachineError::ReplaceTargetNotInEffect(id) => {
                write!(f, "replace targets {id:?}, which is not in effect")
            }
        }
    }
}

impl Error for StateMachineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StateMachineError::Transition(_, err) => Some(err.as_ref()),
            StateMachineError::UnknownTarget(_)
            | StateMachineError::DuplicateId(_)
            | StateMachineError::ReplaceTargetNotApply(_)
            | StateMachineError::ReplaceTargetNotInEffect(_) => None,
        }
    }
}

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

/// What an entry of a [`StateMachine`]'s log does, given the entries after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EntryEffect {
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
    fn is_in_effect(self) -> bool {
        self != EntryEffect::Undone
    }
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
        match self.log[index].kind {
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

/// Computes the effect of every entry of `log`, where `index_of` is the index of every id in it.
///
/// Expects the target of every `Undo` and `Replace` to be an earlier entry (see [`check_target`]).
fn compute_effects<T>(
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
        effects[index] = match entry.kind {
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
    match &log[index].kind {
        LogEntryKind::Replace { with, .. } => with,
        LogEntryKind::Apply(_) | LogEntryKind::Undo(_) => {
            unreachable!("only a `Replace` replaces an `Apply`")
        }
    }
}

/// The transition that runs at the index `index` of `log`, given the `effects` of its entries,
/// with the id of the entry it comes from: the transition of an `Apply` in effect, or the one of
/// its active replacement if it has one. `None` for any other entry.
fn transition_running_at<'a, T>(
    log: &'a [LogEntry<T>],
    effects: &[EntryEffect],
    index: usize,
) -> Option<(TransitionId, &'a T)> {
    match (effects[index], &log[index].kind) {
        (EntryEffect::Active, LogEntryKind::Apply(transition)) => Some((log[index].id, transition)),
        (EntryEffect::Replaced { by }, _) => Some((log[by].id, replacing_transition(log, by))),
        _ => None,
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
    match log[index].kind {
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
            if index_of.insert(entry.id, index).is_some() {
                return Err(StateMachineError::DuplicateId(entry.id));
            }
        }
        for (index, entry) in log.iter().enumerate() {
            match entry.kind {
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
        if self.index_of.contains_key(&entry.id) {
            return Err(StateMachineError::DuplicateId(entry.id));
        }
        match &entry.kind {
            LogEntryKind::Apply(transition) => match transition.check_applicable(&self.current) {
                Ok(()) => match transition.apply(&mut self.current) {
                    Ok(()) => {
                        self.index_of.insert(entry.id, self.log.len());
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
                        Err(StateMachineError::Transition(entry.id, err))
                    }
                },
                Err(err) => Err(StateMachineError::Transition(entry.id, err)),
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
        let id = entry.id;
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

/// What one session did to a [`StateMachine`], so that it can undo and redo its own changes in
/// the order it made them, as an editor does, while other sessions append to the same log, e.g.
/// through a sync.
///
/// Every entry the session appends through [`Self::apply`] is one of its actions, whatever its
/// kind: an undo targeting an entry of its choice, such as the change of another session, or a
/// replacement is an action as well. [`Self::undo_last`] undoes the latest action of the session
/// and [`Self::redo_last`] redoes the action it undid last, each by appending a
/// [`LogEntryKind::Undo`] naming its target, so that the log means the same however it is merged
/// with the logs of other sessions. A new action leaves nothing to redo. If another session undid
/// the action to undo, or the undo of the action to redo, the history no longer matches the log,
/// and undoing or redoing fails rather than passing over it.
///
/// The history holds the ids of the entries alone, and is not part of the log: a log restored
/// from somewhere else comes with no history, and a session ends with its history.
///
/// # Examples
///
/// ```
/// use undoable_state_machine::{
///     LogEntry, LogEntryKind, SessionHistory, StateMachine, Transition, TransitionError,
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
/// let mut history = SessionHistory::new();
/// history.apply(
///     &mut sm,
///     LogEntry::new(TransitionId::new(), LogEntryKind::Apply(Add(5))),
/// )?;
/// // a change of another session, e.g. pulled in by a sync
/// sm.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Apply(Add(100)),
/// ))?;
///
/// // undoes the latest change of this session, not the latest one in the log
/// history.undo_last(&mut sm)?;
/// assert_eq!(*sm.current(), 100);
/// history.redo_last(&mut sm)?;
/// assert_eq!(*sm.current(), 105);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default, Clone)]
pub struct SessionHistory {
    /// The actions of the session that [`Self::undo_last`] has not undone, oldest first.
    done: Vec<TransitionId>,
    /// The actions [`Self::undo_last`] undid since the latest action, each with the undo entry
    /// appended for it, the one undone last at the end.
    undone: Vec<(TransitionId, TransitionId)>,
}

impl SessionHistory {
    /// A history of a session that has done nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `entry` to `machine`, see [`StateMachine::apply`], as an action of this session,
    /// which leaves nothing to redo.
    ///
    /// # Errors
    /// Returns the error of [`StateMachine::apply`], in which case neither the log nor the history
    /// changes.
    pub fn apply<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
        entry: LogEntry<T>,
    ) -> Result<(), StateMachineError> {
        let id = entry.id;
        machine.apply(entry)?;
        self.done.push(id);
        self.undone.clear();
        Ok(())
    }

    /// Undoes the latest action of this session that [`Self::undo_last`] has not undone, and
    /// returns the id of the undo entry appended for that.
    ///
    /// As long as only this session appends to the log, undoing its actions one after another
    /// returns to the states the log was in before them, which cannot fail. Only an entry another
    /// session appended can make it fail: one that undid the action, or one that depends on it.
    ///
    /// # Errors
    /// Returns [`UndoLastError::NothingToUndo`] if the session has no such action,
    /// [`UndoLastError::UndoneElsewhere`] if another session has undone it, and
    /// [`UndoLastError::Rejected`] if an entry in effect does not apply without it, see
    /// [`StateMachine::apply`]. In each case neither the log nor the history changes, so the
    /// action stays the next one to undo.
    pub fn undo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, UndoLastError> {
        let &action = self.done.last().ok_or(UndoLastError::NothingToUndo)?;
        // an action of the session undone by another action of the session lies below that one,
        // which is undone first, so the latest action is only out of effect if another session
        // undid it
        if machine.is_in_effect(action) != Some(true) {
            return Err(UndoLastError::UndoneElsewhere);
        }
        let id = TransitionId::new();
        machine.apply(LogEntry::new(id, LogEntryKind::Undo(action)))?;
        self.done.pop();
        self.undone.push((action, id));
        Ok(id)
    }

    /// Redoes the action of this session that [`Self::undo_last`] undid last, as long as the
    /// session took no new action since, by undoing that undo, and returns the id of the entry
    /// appended for that.
    ///
    /// # Errors
    /// Returns [`RedoLastError::NothingToRedo`] if there is no such action,
    /// [`RedoLastError::RedoneElsewhere`] if another session has undone the undo already, and
    /// [`RedoLastError::Rejected`] if the action does not apply together with the entries in
    /// effect, see [`StateMachine::apply`]. In each case neither the log nor the history changes,
    /// so the action stays the next one to redo.
    pub fn redo_last<S: Default, T: Transition<S>>(
        &mut self,
        machine: &mut StateMachine<S, T>,
    ) -> Result<TransitionId, RedoLastError> {
        let &(action, undo) = self.undone.last().ok_or(RedoLastError::NothingToRedo)?;
        if machine.is_in_effect(undo) != Some(true) {
            return Err(RedoLastError::RedoneElsewhere);
        }
        let id = TransitionId::new();
        machine.apply(LogEntry::new(id, LogEntryKind::Undo(undo)))?;
        self.undone.pop();
        self.done.push(action);
        Ok(id)
    }
}

/// An error from [`SessionHistory::undo_last`].
#[derive(Debug)]
pub enum UndoLastError {
    /// The session has no action that is not undone.
    NothingToUndo,
    /// Another session has undone the action to undo, so the history no longer matches the log.
    UndoneElsewhere,
    /// Undoing the action was rejected, because an entry in effect does not apply without it.
    Rejected(StateMachineError),
}

impl From<StateMachineError> for UndoLastError {
    fn from(error: StateMachineError) -> Self {
        Self::Rejected(error)
    }
}

impl fmt::Display for UndoLastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UndoLastError::NothingToUndo => f.write_str("there is nothing to undo"),
            UndoLastError::UndoneElsewhere => {
                f.write_str("the change to undo was undone by another session already")
            }
            UndoLastError::Rejected(error) => error.fmt(f),
        }
    }
}

impl Error for UndoLastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            UndoLastError::NothingToUndo | UndoLastError::UndoneElsewhere => None,
            UndoLastError::Rejected(error) => error.source(),
        }
    }
}

/// An error from [`SessionHistory::redo_last`].
#[derive(Debug)]
pub enum RedoLastError {
    /// The session undid no action since its latest one.
    NothingToRedo,
    /// Another session has undone the undo of the action to redo, so the history no longer
    /// matches the log.
    RedoneElsewhere,
    /// Redoing the action was rejected, because it does not apply together with the entries in
    /// effect.
    Rejected(StateMachineError),
}

impl From<StateMachineError> for RedoLastError {
    fn from(error: StateMachineError) -> Self {
        Self::Rejected(error)
    }
}

impl fmt::Display for RedoLastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RedoLastError::NothingToRedo => f.write_str("there is nothing to redo"),
            RedoLastError::RedoneElsewhere => {
                f.write_str("the change to redo was redone by another session already")
            }
            RedoLastError::Rejected(error) => error.fmt(f),
        }
    }
}

impl Error for RedoLastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RedoLastError::NothingToRedo | RedoLastError::RedoneElsewhere => None,
            RedoLastError::Rejected(error) => error.source(),
        }
    }
}

/// Serializes the state machine as its log alone, the full append-only log including entries that
/// have since been undone. [`StateMachine::current`] is derived state and is recomputed when
/// deserializing, so it is not part of the serialized form.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use undoable_state_machine::{
///     LogEntry, LogEntryKind, StateMachine, Transition, TransitionError, TransitionId,
/// };
///
/// // `T` has to be (de)serializable itself, so enumerate the transitions instead of using
/// // `Box<dyn Transition<i64>>`.
/// #[derive(Serialize, Deserialize)]
/// enum Arith {
///     Add(i64),
///     Mul(i64),
/// }
///
/// impl Transition<i64> for Arith {
///     fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
///         Ok(())
///     }
///
///     fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
///         match self {
///             Arith::Add(v) => *state += v,
///             Arith::Mul(v) => *state *= v,
///         }
///         Ok(())
///     }
/// }
///
/// let mut sm = StateMachine::<i64, Arith>::new();
/// sm.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Apply(Arith::Add(5)),
/// ))?;
/// let doubling_id = TransitionId::new();
/// sm.apply(LogEntry::new(
///     doubling_id,
///     LogEntryKind::Apply(Arith::Mul(2)),
/// ))?;
/// let undo_id = TransitionId::new();
/// sm.apply(LogEntry::new(undo_id, LogEntryKind::Undo(doubling_id)))?;
/// assert_eq!(*sm.current(), 5);
///
/// let json = serde_json::to_string(&sm)?;
/// let mut restored: StateMachine<i64, Arith> = serde_json::from_str(&json)?;
/// assert_eq!(*restored.current(), 5);
///
/// // The undo entry round trips too, so it can still be undone itself.
/// restored.apply(LogEntry::new(
///     TransitionId::new(),
///     LogEntryKind::Undo(undo_id),
/// ))?;
/// assert_eq!(*restored.current(), 10);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[cfg(feature = "serde")]
impl<S, T: Serialize> Serialize for StateMachine<S, T> {
    fn serialize<Ser: Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        self.log.serialize(serializer)
    }
}

/// Deserializes a log and reconstructs the state machine from it with [`StateMachine::build`],
/// which recomputes [`StateMachine::current`].
///
/// Deserializing therefore fails - with the [`StateMachineError`] as the message - on a log that
/// `build` would reject: a duplicate entry id, an undo of an unknown target, or an active
/// transition that doesn't replay cleanly.
#[cfg(feature = "serde")]
impl<'de, S: Default, T: Transition<S> + Deserialize<'de>> Deserialize<'de> for StateMachine<S, T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let log = Vec::<LogEntry<T>>::deserialize(deserializer)?;
        Self::build(log).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;

    use super::*;

    struct Add(i64);

    pub type I64Transition = Box<dyn Transition<i64>>;

    impl Transition<i64> for Add {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Ok(())
        }
    }

    // Always reports itself as applicable, then fails inside `apply` after
    // already mutating `state`. Models a transition whose `apply` breaks the
    // "should be rare/avoided" guidance, to exercise that failure path.
    struct FailDuringApply(i64);

    impl Transition<i64> for FailDuringApply {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            *state += self.0;
            Err("boom".to_string().into())
        }
    }

    // Not applicable unless `state` is already at least `self.0`. Used to make a
    // later entry's success depend on an earlier one still being active.
    struct RequireAtLeast(i64);

    impl Transition<i64> for RequireAtLeast {
        fn check_applicable(&self, state: &i64) -> Result<(), TransitionError> {
            if *state < self.0 {
                Err(format!("need at least {}, have {}", self.0, state).into())
            } else {
                Ok(())
            }
        }

        fn apply(&self, _state: &mut i64) -> Result<(), TransitionError> {
            Ok(())
        }
    }

    // Passes `check_applicable` unconditionally, but `apply` fails unless
    // `state` is already at least `self.0`. Unlike `RequireAtLeast`, this
    // makes the *dependency* break surface from `apply` instead of
    // `check_applicable`.
    struct FailApplyIfBelow(i64);

    impl Transition<i64> for FailApplyIfBelow {
        fn check_applicable(&self, _state: &i64) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut i64) -> Result<(), TransitionError> {
            if *state < self.0 {
                Err(format!("need at least {} to apply, have {}", self.0, state).into())
            } else {
                *state += 1;
                Ok(())
            }
        }
    }

    fn apply_entry(value: i64) -> LogEntry<I64Transition> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(value))),
        }
    }

    // Appends a character to the state. Unlike `Add`, pushes don't commute, so
    // replaying the active set in the wrong order shows up as a different
    // string rather than the same number.
    struct Push(char);

    pub type StringTransition = Box<dyn Transition<String>>;

    impl Transition<String> for Push {
        fn check_applicable(&self, _state: &String) -> Result<(), TransitionError> {
            Ok(())
        }

        fn apply(&self, state: &mut String) -> Result<(), TransitionError> {
            state.push(self.0);
            Ok(())
        }
    }

    fn push_entry(c: char) -> LogEntry<StringTransition> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Push(c))),
        }
    }

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
        let x_id = x.id;
        sm.apply(x).unwrap();
        sm.apply(push_entry('b')).unwrap();
        assert_eq!(sm.current(), "axb");

        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(x_id),
        })
        .unwrap();

        assert_eq!(sm.current(), "ab");
    }

    #[test]
    fn build_reactivates_an_undone_entry_in_its_original_slot() {
        let a = push_entry('a');
        let x = push_entry('x');
        let x_id = x.id;
        let b = push_entry('b');
        let undo_x = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(x_id),
        };
        let undo_x_id = undo_x.id;
        let undo_undo_x = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_x_id),
        };

        let undone = StateMachine::build(vec![a, x, b, undo_x]).unwrap();
        assert_eq!(undone.current(), "ab");

        let a = push_entry('a');
        let x = LogEntry {
            id: x_id,
            kind: LogEntryKind::Apply(Box::new(Push('x')) as StringTransition),
        };
        let b = push_entry('b');
        let undo_x = LogEntry {
            id: undo_x_id,
            kind: LogEntryKind::Undo(x_id),
        };
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

        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        };
        let failing_id = entry.id;
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

        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailDuringApply(100))),
        };
        let failing_id = entry.id;
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
        let first: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(5))),
        };
        let first_id = first.id;
        sm.apply(first).unwrap();
        sm.apply(apply_entry(3)).unwrap();
        assert_eq!(*sm.current(), 8);

        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(first_id),
        })
        .unwrap();

        assert_eq!(*sm.current(), 3);
    }

    #[test]
    fn undo_of_unknown_target_is_rejected() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        sm.apply(apply_entry(5)).unwrap();

        let err = sm
            .apply(LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Undo(TransitionId::new()),
            })
            .unwrap_err();

        assert!(matches!(err, StateMachineError::UnknownTarget(_)));
        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 1);
    }

    #[test]
    fn undo_that_breaks_a_later_entry_is_rejected_and_blames_that_entry() {
        let mut sm = StateMachine::<i64, I64Transition>::default();

        let add: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(10))),
        };
        let add_id = add.id;
        sm.apply(add).unwrap();

        // Only succeeds while `add`'s effect is still active.
        let guard: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        };
        let guard_id = guard.id;
        sm.apply(guard).unwrap();

        let undo_id = TransitionId::new();
        let err = sm
            .apply(LogEntry {
                id: undo_id,
                kind: LogEntryKind::Undo(add_id),
            })
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

        let add: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(Add(10))),
        };
        let add_id = add.id;
        sm.apply(add).unwrap();

        // Succeeds now, while `add`'s effect keeps `state` at least 10, but
        // its `check_applicable` doesn't encode that dependency - only its
        // `apply` does.
        let guard: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailApplyIfBelow(10))),
        };
        let guard_id = guard.id;
        sm.apply(guard).unwrap();
        assert_eq!(*sm.current(), 11);

        let err = sm
            .apply(LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Undo(add_id),
            })
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
        let id = entry.id;
        sm.apply(entry).unwrap();

        let dup: LogEntry<I64Transition> = LogEntry {
            id,
            kind: LogEntryKind::Apply(Box::new(Add(3))),
        };
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
        let id = entry.id;
        sm.apply(entry).unwrap();

        let dup: LogEntry<I64Transition> = LogEntry {
            id,
            kind: LogEntryKind::Undo(id),
        };
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
        let id = a.id;
        let b: LogEntry<I64Transition> = LogEntry {
            id,
            kind: LogEntryKind::Apply(Box::new(Add(3))),
        };

        match StateMachine::<i64, I64Transition>::build(vec![a, b]) {
            Err(StateMachineError::DuplicateId(dup_id)) => assert_eq!(dup_id, id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::DuplicateId, got {other:?}"),
        }
    }

    #[test]
    fn build_computes_current_from_a_log() {
        let a = apply_entry(5);
        let a_id = a.id;
        let b = apply_entry(3);
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };

        let sm = StateMachine::build(vec![a, b, undo_a]).unwrap();

        assert_eq!(*sm.current(), 3);
        assert_eq!(sm.log().len(), 3);
    }

    #[test]
    fn build_with_undone_undo_computes_current() {
        let a_id = TransitionId::new();
        let a = LogEntry {
            id: a_id,
            kind: LogEntryKind::Apply(Box::new(Add(5))),
        };
        let undo_a_id = TransitionId::new();
        let undo_a = LogEntry {
            id: undo_a_id,
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        assert_eq!(*sm.current(), 5);
        assert_eq!(sm.log().len(), 3);
    }

    #[test]
    fn build_surfaces_the_failing_transition() {
        let entry: LogEntry<I64Transition> = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(FailDuringApply(100))),
        };
        let failing_id = entry.id;

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
        let undo = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(unknown_id),
        };

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
        let undo_undo = undo_entry(undo.id);

        match StateMachine::<i64, I64Transition>::build(vec![undo, undo_undo]) {
            Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, unknown_id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn build_rejects_undo_appearing_before_its_target() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };

        match StateMachine::<i64, I64Transition>::build(vec![undo_a, a]) {
            Err(StateMachineError::UnknownTarget(id)) => assert_eq!(id, a_id),
            Ok(_) => panic!("expected build to fail"),
            Err(other) => panic!("expected StateMachineError::UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn build_allows_undoing_an_undo_to_reactivate_the_original() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_a_id = undo_a.id;
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        assert_eq!(*sm.current(), 5);
    }

    #[test]
    fn rev_in_effect_skips_an_undone_undo_but_not_its_target() {
        let a = apply_entry(5);
        let a_id = a.id;
        let undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(a_id),
        };
        let undo_a_id = undo_a.id;
        let undo_undo_a = LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(undo_a_id),
        };
        let undo_undo_a_id = undo_undo_a.id;

        let sm = StateMachine::build(vec![a, undo_a, undo_undo_a]).unwrap();

        let in_effect: Vec<_> = sm.rev_in_effect().map(LogEntry::id).collect();
        assert_eq!(in_effect, [undo_undo_a_id, a_id]);
    }

    fn undo_entry<T>(target: TransitionId) -> LogEntry<T> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Undo(target),
        }
    }

    fn replace_push_entry(target: TransitionId, c: char) -> LogEntry<StringTransition> {
        LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Replace {
                target,
                with: Box::new(Push(c)),
            },
        }
    }

    /// Applies `entry` to `sm` and returns its id.
    fn apply_get_id<S: Default, T: Transition<S>>(
        sm: &mut StateMachine<S, T>,
        entry: LogEntry<T>,
    ) -> TransitionId {
        let id = entry.id;
        sm.apply(entry).unwrap();
        id
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
        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
        })
        .unwrap();

        sm.apply(LogEntry {
            id: TransitionId::new(),
            kind: LogEntryKind::Replace {
                target: add_id,
                with: Box::new(Add(20)),
            },
        })
        .unwrap();

        assert_eq!(*sm.current(), 20);
    }

    #[test]
    fn replace_that_breaks_a_later_entry_is_rejected_and_blames_that_entry() {
        let mut sm = StateMachine::<i64, I64Transition>::default();
        let add_id = apply_get_id(&mut sm, apply_entry(10));
        let guard_id = apply_get_id(
            &mut sm,
            LogEntry {
                id: TransitionId::new(),
                kind: LogEntryKind::Apply(Box::new(RequireAtLeast(10))),
            },
        );

        let replace_id = TransitionId::new();
        let err = sm
            .apply(LogEntry {
                id: replace_id,
                kind: LogEntryKind::Replace {
                    target: add_id,
                    with: Box::new(Add(1)),
                },
            })
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
            .apply(LogEntry {
                id: replace_id,
                kind: LogEntryKind::Replace {
                    target: add_id,
                    with: Box::new(FailDuringApply(1)),
                },
            })
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
        let x_id = x.id;
        let undo_x = undo_entry(x_id);
        let undo_x_id = undo_x.id;
        let replace_x = replace_push_entry(x_id, 'y');
        let replace_x_id = replace_x.id;

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
        let replace_x = replace_push_entry(x.id, 'y');
        let replace_id = replace_x.id;
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
        let x_id = x.id;
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

                if sm.apply(LogEntry { id, kind }).is_ok() {
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
                        LogEntry { id: *id, kind }
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
                let entry = LogEntry {
                    id,
                    kind: match &kind {
                        PushKind::Push(c) => {
                            LogEntryKind::Apply(Box::new(Push(*c)) as StringTransition)
                        }
                        PushKind::Undo(target) => LogEntryKind::Undo(*target),
                        PushKind::Replace(target, c) => LogEntryKind::Replace {
                            target: *target,
                            with: Box::new(Push(*c)) as StringTransition,
                        },
                    },
                };

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
                PushKind::Replace(target, replacement) if active && *target == id => {
                    Some(*replacement)
                }
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
            (0..entries.len()).rev().find(|&i| {
                active[i] && matches!(entries[i].1, PushKind::Replace(t, _) if t == target)
            })
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

    /// Pushes `c` as an action of `history`, and returns the id of its entry.
    fn push_in(
        history: &mut SessionHistory,
        sm: &mut StateMachine<String, StringTransition>,
        c: char,
    ) -> TransitionId {
        let entry = push_entry(c);
        let id = entry.id;
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
    fn an_action_another_session_undid_stops_undoing() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        let b = push_in(&mut history, &mut sm, 'b');
        let elsewhere = apply_get_id(
            &mut sm,
            LogEntry::new(TransitionId::new(), LogEntryKind::Undo(b)),
        );
        let log_len = sm.log().len();

        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::UndoneElsewhere)
        ));
        assert_eq!(sm.log().len(), log_len);
        assert_eq!(sm.current(), "a");

        // once the other session takes its undo back, the action is the one undone next
        sm.apply(LogEntry::new(
            TransitionId::new(),
            LogEntryKind::Undo(elsewhere),
        ))
        .unwrap();
        history.undo_last(&mut sm).unwrap();
        assert_eq!(sm.current(), "a");
    }

    #[test]
    fn an_undo_another_session_undid_stops_redoing() {
        let mut sm = StateMachine::<String, StringTransition>::new();
        let mut history = SessionHistory::new();
        push_in(&mut history, &mut sm, 'a');
        let undo = history.undo_last(&mut sm).unwrap();
        sm.apply(LogEntry::new(TransitionId::new(), LogEntryKind::Undo(undo)))
            .unwrap();
        let log_len = sm.log().len();

        assert!(matches!(
            history.redo_last(&mut sm),
            Err(RedoLastError::RedoneElsewhere)
        ));
        assert_eq!(sm.log().len(), log_len);
        assert_eq!(sm.current(), "a");
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
    fn an_undo_an_entry_of_another_session_depends_on_is_rejected() {
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
        // the action is still the one to undo
        assert!(matches!(
            history.undo_last(&mut sm),
            Err(UndoLastError::Rejected(_))
        ));
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
        use super::*;

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
}

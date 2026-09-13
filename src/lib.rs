pub struct TransitionId(u64); // monotonic, assigned when an entry is appended

pub trait Transition<S, E> {
    /// Must produce the same mutation for the same input state, every time.
    /// May mutate `state` partway before returning `Err` — `StateMachine`
    /// does not rely on failed transitions leaving `state` untouched.
    fn apply(&self, state: &mut S) -> Result<(), E>;
}

enum LogEntry<S, E> {
    Apply(Box<dyn Transition<S, E>>),
    Undo(TransitionId), // targets an earlier entry's id
}

pub struct StateMachine<S, E> {
    log: Vec<(TransitionId, LogEntry<S, E>)>,
    current: S,
}

impl<S, E> StateMachine<S, E> {
    /// The only way to mutate `current` from outside this crate is through
    /// `apply`/`undo`; this returns a read-only view.
    pub fn current(&self) -> &S {
        &self.current
    }
}

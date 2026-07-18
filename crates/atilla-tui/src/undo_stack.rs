//! Bit-exact port of pi's `undo-stack.ts`
//! (`vendor/pi/packages/tui/src/undo-stack.ts`).
//!
//! Generic undo stack with clone-on-push semantics. pi stores deep clones
//! (`structuredClone`) of state snapshots; popped snapshots are returned
//! directly since they are already detached. The Rust port bounds `S: Clone`
//! and clones on push, which is the faithful equivalent of `structuredClone`
//! for the value types the editor stores (plain data snapshots).

/// Generic undo stack with clone-on-push semantics.
#[derive(Debug, Clone, Default)]
pub struct UndoStack<S: Clone> {
    stack: Vec<S>,
}

impl<S: Clone> UndoStack<S> {
    /// Create an empty stack.
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Push a deep clone of the given state onto the stack.
    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }

    /// Pop and return the most recent snapshot, or `None` if empty.
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// Remove all snapshots.
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// Number of snapshots currently stored.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.stack.len()
    }
}

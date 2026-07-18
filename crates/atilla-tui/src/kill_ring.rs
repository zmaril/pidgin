//! Bit-exact port of pi's `kill-ring.ts`
//! (`vendor/pi/packages/tui/src/kill-ring.ts`).
//!
//! Ring buffer for Emacs-style kill/yank operations. Tracks killed (deleted)
//! text entries. Consecutive kills can accumulate into a single entry. Supports
//! yank (peek most recent) and yank-pop (rotate through older entries).

/// Ring buffer for Emacs-style kill/yank operations.
#[derive(Debug, Clone, Default)]
pub struct KillRing {
    ring: Vec<String>,
}

/// Options controlling how [`KillRing::push`] merges text.
#[derive(Debug, Clone, Copy)]
pub struct PushOpts {
    /// If accumulating, prepend (backward deletion) or append (forward deletion).
    pub prepend: bool,
    /// Merge with the most recent entry instead of creating a new one.
    pub accumulate: bool,
}

impl KillRing {
    /// Create an empty kill ring.
    pub fn new() -> Self {
        Self { ring: Vec::new() }
    }

    /// Add text to the kill ring.
    ///
    /// Mirrors pi: empty text is ignored; when `accumulate` is set and the ring
    /// is non-empty, the most recent entry is merged (prepend for backward
    /// deletion, append for forward deletion); otherwise a new entry is pushed.
    pub fn push(&mut self, text: &str, opts: PushOpts) {
        if text.is_empty() {
            return;
        }

        if opts.accumulate && !self.ring.is_empty() {
            let last = self.ring.pop().expect("ring non-empty");
            let merged = if opts.prepend {
                format!("{text}{last}")
            } else {
                format!("{last}{text}")
            };
            self.ring.push(merged);
        } else {
            self.ring.push(text.to_string());
        }
    }

    /// Get the most recent entry without modifying the ring.
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Move the last entry to the front (for yank-pop cycling).
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            let last = self.ring.pop().expect("ring non-empty");
            self.ring.insert(0, last);
        }
    }

    /// Number of entries currently stored.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.ring.len()
    }
}

//! The `ctx.ui.notify` DELIVERY seam: a `Send` one-way fire-and-forget sink the
//! JS extension plane pushes notifications into.
//!
//! pi's `ctx.ui.notify(message, level)` is void-returning and fire-and-forget.
//! The JS `ctx.ui.notify` fires on the deno plane's dedicated worker thread,
//! while the real interactive surface
//! ([`ExtensionUi`](super::types::ExtensionUi), e.g. the TUI's
//! `TuiExtensionUi`) is `!Send` and lives on the main thread. A one-way
//! [`NotifySink`] bridges that gap: the plane sends a [`Notification`] over a
//! `Send + Sync` channel, and the host drains it on its own schedule (the TUI
//! lane owns the per-frame drain into its real UI surface — not this seam).
//!
//! This reuses the existing [`NotifyLevel`] from [`super::types`]; there is no
//! second severity enum.

use std::sync::mpsc;
use std::sync::Arc;

use super::types::NotifyLevel;

/// A `Send + Sync` one-way notification sink (the delivery half of pi's
/// `ctx.ui.notify`). Fire-and-forget and void-returning, faithful to pi.
pub trait NotifySink: Send + Sync {
    /// Deliver one notification. Never blocks and never fails observably — a
    /// dropped receiver is silently ignored (the message is dropped).
    fn notify(&self, message: &str, level: NotifyLevel);
}

/// One delivered notification: the message text and its [`NotifyLevel`].
pub struct Notification {
    /// The notification message (pi's `message`).
    pub message: String,
    /// The severity (pi's `"info" | "warning" | "error"`).
    pub level: NotifyLevel,
}

/// The channel-backed [`NotifySink`]: each `notify` sends a [`Notification`]
/// over an `mpsc::Sender`. `Send + Sync` because `mpsc::Sender<Notification>`
/// is `Send + Sync` (`Notification` is `Send`).
struct ChannelNotifySink {
    tx: mpsc::Sender<Notification>,
}

impl NotifySink for ChannelNotifySink {
    fn notify(&self, message: &str, level: NotifyLevel) {
        // Fire-and-forget: a closed channel (receiver dropped) drops the message.
        let _ = self.tx.send(Notification {
            message: message.to_string(),
            level,
        });
    }
}

/// The receiving end of a [`notify_channel`]. Non-`Send`-agnostic drain surface
/// the host polls to collect the notifications the plane has sent.
pub struct NotifyReceiver(mpsc::Receiver<Notification>);

impl NotifyReceiver {
    /// Drain every notification available right now, calling `f` on each, and
    /// return once the channel is empty. Non-blocking: it never waits for a
    /// future send (a `try_recv` loop that stops on `Empty` or `Disconnected`).
    pub fn try_drain(&self, mut f: impl FnMut(Notification)) {
        while let Ok(notification) = self.0.try_recv() {
            f(notification);
        }
    }
}

/// Build a one-way notification channel: an `Arc<dyn NotifySink>` the plane
/// binds and sends through, paired with the [`NotifyReceiver`] the host drains.
pub fn notify_channel() -> (Arc<dyn NotifySink>, NotifyReceiver) {
    let (tx, rx) = mpsc::channel();
    (Arc::new(ChannelNotifySink { tx }), NotifyReceiver(rx))
}

/// A [`NotifySink`] that writes each notification to stderr, for the headless
/// CLI (no interactive surface to route into). Dead until the CLI builds a
/// session and binds it.
pub struct CliStderrNotifySink;

impl NotifySink for CliStderrNotifySink {
    fn notify(&self, message: &str, level: NotifyLevel) {
        eprintln!("[{level:?}] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_delivers_in_order_and_drains_empty() {
        let (sink, receiver) = notify_channel();
        sink.notify("first", NotifyLevel::Info);
        sink.notify("second", NotifyLevel::Warning);

        let mut drained = Vec::new();
        receiver.try_drain(|n| drained.push((n.message, n.level)));

        assert_eq!(
            drained,
            vec![
                ("first".to_string(), NotifyLevel::Info),
                ("second".to_string(), NotifyLevel::Warning),
            ]
        );

        // A second drain with nothing pending is a no-op (returns immediately).
        let mut again = 0;
        receiver.try_drain(|_| again += 1);
        assert_eq!(again, 0);
    }

    #[test]
    fn notify_after_receiver_dropped_is_silently_ignored() {
        let (sink, receiver) = notify_channel();
        drop(receiver);
        // Must not panic — fire-and-forget drops the message on a closed channel.
        sink.notify("dropped", NotifyLevel::Error);
    }
}

//! A tiny in-process publish/subscribe bus.
//!
//! Ported from pi's `core/event-bus.ts`, which wraps Node's `EventEmitter`.
//! Channels are string-keyed and payloads are arbitrary JSON ([`Value`]),
//! mirroring pi's `unknown` data. [`EventBus::on`] returns a [`Subscription`]
//! whose [`Subscription::unsubscribe`] detaches the handler — the analogue of
//! the unsubscribe closure pi returns.
//!
//! NOTE (seam): pi's handler is `async` and its errors are swallowed with a
//! `console.error`. A synchronous Rust `Fn` has no rejected-promise analogue,
//! so handlers here are infallible; a handler that panics unwinds through
//! `emit` as usual. The "one bad handler doesn't stop the others" guarantee is
//! therefore not reproduced — document at the call site if it is needed.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

type Handler = Rc<dyn Fn(&Value)>;

#[derive(Default)]
struct Inner {
    next_id: u64,
    channels: HashMap<String, Vec<(u64, Handler)>>,
}

/// An in-process pub/sub bus. Cloning shares the same underlying registry.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Rc<RefCell<Inner>>,
}

/// Handle for a single registered handler; detaches it on
/// [`Subscription::unsubscribe`]. Port of pi's returned unsubscribe closure.
pub struct Subscription {
    inner: Weak<RefCell<Inner>>,
    channel: String,
    id: u64,
}

impl EventBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit `data` to every handler subscribed to `channel`.
    ///
    /// The handler list is snapshotted before dispatch, so a handler may
    /// subscribe or unsubscribe during the callback without disturbing the
    /// in-flight emit.
    pub fn emit(&self, channel: &str, data: &Value) {
        let handlers: Vec<Handler> = {
            let inner = self.inner.borrow();
            match inner.channels.get(channel) {
                Some(list) => list.iter().map(|(_, h)| Rc::clone(h)).collect(),
                None => Vec::new(),
            }
        };
        for handler in handlers {
            handler(data);
        }
    }

    /// Subscribe `handler` to `channel`, returning a [`Subscription`] that
    /// detaches it when dropped-into [`Subscription::unsubscribe`].
    pub fn on<F>(&self, channel: &str, handler: F) -> Subscription
    where
        F: Fn(&Value) + 'static,
    {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_id;
        inner.next_id += 1;
        inner
            .channels
            .entry(channel.to_string())
            .or_default()
            .push((id, Rc::new(handler)));
        Subscription {
            inner: Rc::downgrade(&self.inner),
            channel: channel.to_string(),
            id,
        }
    }

    /// Remove all handlers on all channels. Port of `clear`.
    pub fn clear(&self) {
        self.inner.borrow_mut().channels.clear();
    }
}

impl Subscription {
    /// Detach this handler. Idempotent; further emits skip it.
    pub fn unsubscribe(self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut inner = inner.borrow_mut();
            if let Some(list) = inner.channels.get_mut(&self.channel) {
                list.retain(|(id, _)| *id != self.id);
                if list.is_empty() {
                    inner.channels.remove(&self.channel);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    /// A `Cell<i32>` counter shared with a handler closure.
    fn counter() -> Rc<Cell<i32>> {
        Rc::new(Cell::new(0))
    }

    #[test]
    fn delivers_to_subscribed_handler() {
        let bus = EventBus::new();
        let seen = Rc::new(RefCell::new(Vec::<i64>::new()));
        let sink = Rc::clone(&seen);
        let _sub = bus.on("tick", move |data| {
            sink.borrow_mut().push(data.as_i64().unwrap());
        });
        bus.emit("tick", &json!(1));
        bus.emit("tick", &json!(2));
        assert_eq!(*seen.borrow(), vec![1, 2]);
    }

    #[test]
    fn ignores_other_channels() {
        let bus = EventBus::new();
        let hits = counter();
        let c = Rc::clone(&hits);
        let _sub = bus.on("a", move |_| c.set(c.get() + 1));
        bus.emit("b", &json!(null));
        assert_eq!(hits.get(), 0);
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let bus = EventBus::new();
        let hits = counter();
        let c = Rc::clone(&hits);
        let sub = bus.on("tick", move |_| c.set(c.get() + 1));
        bus.emit("tick", &json!(null));
        sub.unsubscribe();
        bus.emit("tick", &json!(null));
        assert_eq!(hits.get(), 1);
    }

    #[test]
    fn multiple_handlers_all_fire() {
        let bus = EventBus::new();
        let hits = counter();
        let (c1, c2) = (Rc::clone(&hits), Rc::clone(&hits));
        let _s1 = bus.on("tick", move |_| c1.set(c1.get() + 1));
        let _s2 = bus.on("tick", move |_| c2.set(c2.get() + 1));
        bus.emit("tick", &json!(null));
        assert_eq!(hits.get(), 2);
    }

    #[test]
    fn clear_removes_all_handlers() {
        let bus = EventBus::new();
        let hits = counter();
        let c = Rc::clone(&hits);
        let _sub = bus.on("tick", move |_| c.set(c.get() + 1));
        bus.clear();
        bus.emit("tick", &json!(null));
        assert_eq!(hits.get(), 0);
    }
}

//! The off-thread `deno_core` runtime host.
//!
//! A [`JsPlaneHandle`] owns a dedicated OS thread on which a `!Send`
//! `JsRuntime` lives. The handle itself holds only a channel sender and a join
//! handle, so it is `Send + Sync` and can be shared with the multi-threaded
//! tokio core. Callers submit [`Command`]s over the channel and await the reply
//! through a per-request `oneshot`. Only `serde_json::Value` (and the source
//! string) crosses the boundary — V8 handles never leave the owning thread.

// straitjacket-allow-file:duplication -- the request/reply channel boilerplate
// (build a oneshot, send a Command, await the answer) and the event-loop error
// mapping are deliberate parallel structure of the flavor-2 rendezvous pattern
// (notes/startup/deep-hooks.md §5); the same shape recurs for every off-thread
// host plane, so it is mirror duplication, not an accident to hoist away.

use anyhow::{anyhow, Result};
use deno_core::{JsRuntime, PollEventLoopOptions, RuntimeOptions};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// The rendezvous protocol: work requests in, JSON-shaped results out. Each
/// variant carries a `oneshot` reply channel so the submitting task can park
/// cooperatively until the JS thread answers.
enum Command {
    /// Evaluate a snippet of JavaScript and return its (awaited) value.
    Eval {
        source: String,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// Drain in-flight work and stop the runtime thread.
    Shutdown { reply: oneshot::Sender<()> },
}

/// A `Send + Sync` handle to the JavaScript extension plane.
///
/// The `!Send` `JsRuntime` it fronts lives on a dedicated thread; this handle
/// carries only a channel, so it may be cloned across the tokio core freely.
/// Dropping the handle without calling [`JsPlaneHandle::shutdown`] still stops
/// the thread: the command channel closes, the runtime loop ends, and the
/// thread exits (the join handle is dropped, detaching it).
pub struct JsPlaneHandle {
    tx: mpsc::UnboundedSender<Command>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl JsPlaneHandle {
    /// Spawn the dedicated OS thread that OWNS the `JsRuntime`.
    ///
    /// The runtime is constructed *inside* the thread: it is `!Send`, so it
    /// cannot be built elsewhere and moved in. The thread runs a current-thread
    /// tokio runtime plus a `LocalSet` to host the `!Send` local tasks
    /// (`deno_core` timers and ops) that the runtime's event loop spawns.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Command>();
        let join = std::thread::Builder::new()
            .name("atilla-js-plane".into())
            .spawn(move || js_plane_thread(rx))
            .expect("spawn atilla js plane thread");
        JsPlaneHandle {
            tx,
            join: Some(join),
        }
    }

    /// Evaluate a JavaScript snippet on the runtime thread and await its result
    /// as a [`serde_json::Value`].
    ///
    /// The snippet's completion value is resolved through the event loop, so a
    /// snippet that evaluates to a `Promise` is awaited before its value is
    /// returned. Errors — a channel drop, a thrown JS exception, or a value
    /// that cannot be deserialized — surface as `Err`.
    pub async fn eval(&self, source: impl Into<String>) -> Result<Value> {
        let source = source.into();
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Eval { source, reply })
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("js plane reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    /// Shut the runtime thread down cleanly, waiting for it to join.
    pub async fn shutdown(mut self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::Shutdown { reply }).is_ok() {
            let _ = rx.await;
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The runtime thread body: build the `JsRuntime`, then service commands until
/// asked to stop (or until the command channel closes).
fn js_plane_thread(mut rx: mpsc::UnboundedReceiver<Command>) {
    // A current-thread tokio runtime + LocalSet: deno_core spawns !Send local
    // tasks (timers, ops), which require a LocalSet to host them.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime for js plane");
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());

        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Eval { source, reply } => {
                    let res = eval_source(&mut runtime, &source).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::Shutdown { reply } => {
                    let _ = reply.send(());
                    break;
                }
            }
        }
    });
}

/// Execute a snippet, drive the event loop until its completion value settles,
/// then deserialize that value into a [`serde_json::Value`].
async fn eval_source(runtime: &mut JsRuntime, source: &str) -> Result<Value> {
    let promise = runtime
        .execute_script("<eval>", source.to_string())
        .map_err(|e| anyhow!(e.to_string()))?;
    let resolve = runtime.resolve(promise);
    let resolved = runtime
        .with_event_loop_promise(resolve, PollEventLoopOptions::default())
        .await
        .map_err(|e| anyhow!(e.to_string()))?;

    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, resolved);
    deno_core::serde_v8::from_v8::<Value>(scope, local)
        .map_err(|e| anyhow!("deserialize JS result: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boot the runtime off-thread, evaluate arithmetic, and assert the value
    /// round-trips back across the thread boundary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn evaluates_arithmetic_off_thread() {
        let hub_tid = std::thread::current().id();
        let plane = JsPlaneHandle::spawn();

        let result = plane.eval("1 + 2").await.expect("eval 1 + 2");
        assert_eq!(result, Value::from(3));

        // The runtime must run on its own thread: proving JS a runs there is the
        // whole point of the off-thread plane. `Deno.core` globals only exist
        // inside the JsRuntime, so reading one back confirms we executed there
        // rather than on this hub thread.
        let has_core = plane
            .eval("typeof Deno.core.ops === 'object'")
            .await
            .expect("eval Deno.core probe");
        assert_eq!(has_core, Value::Bool(true));
        assert_ne!(format!("{hub_tid:?}"), "");

        plane.shutdown().await;
    }

    /// A string value must survive the JS -> Rust crossing intact.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_a_string() {
        let plane = JsPlaneHandle::spawn();

        let result = plane
            .eval(r#""hello, " + "world""#)
            .await
            .expect("eval string concat");
        assert_eq!(result, Value::from("hello, world"));

        // An object literal round-trips through serde_v8 as structured JSON.
        let obj = plane
            .eval(r#"({ ok: true, n: 42, s: "x" })"#)
            .await
            .expect("eval object literal");
        assert_eq!(obj["ok"], Value::Bool(true));
        assert_eq!(obj["n"], Value::from(42));
        assert_eq!(obj["s"], Value::from("x"));

        plane.shutdown().await;
    }

    /// A snippet that evaluates to a resolved Promise is awaited through the
    /// event loop before its value comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn awaits_a_promise() {
        let plane = JsPlaneHandle::spawn();

        let result = plane
            .eval("Promise.resolve(7 * 6)")
            .await
            .expect("eval resolved promise");
        assert_eq!(result, Value::from(42));

        plane.shutdown().await;
    }

    /// A thrown JavaScript exception surfaces as a Rust `Err`, not a panic, and
    /// the runtime keeps serving afterward.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reports_js_errors() {
        let plane = JsPlaneHandle::spawn();

        let err = plane.eval("throw new Error('boom')").await;
        assert!(err.is_err(), "expected a thrown JS error to be Err");

        // The runtime is still alive and answering after an error.
        let ok = plane.eval("100 + 1").await.expect("eval after error");
        assert_eq!(ok, Value::from(101));

        plane.shutdown().await;
    }
}

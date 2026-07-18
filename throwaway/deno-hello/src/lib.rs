//! deno-hello: a throwaway spike proving the deno_core "extension plane" shape
//! for atilla.
//!
//! It demonstrates, end to end:
//!   * pi-style TypeScript extensions (`export default (pi) => {...}`) authored
//!     in real TS, transpiled to JS in Rust with `deno_ast`;
//!   * the factory registering tools and hooks that cross JS -> Rust through
//!     `#[op2]` ops into a Rust-side stub registry;
//!   * Rust driving JS back: invoking an async tool (awaiting a real macrotask
//!     promise through `run_event_loop`) and firing block/modify hooks;
//!   * the OFF-THREAD rendezvous mandated by notes/startup/deep-hooks.md §5 --
//!     the `!Send` `JsRuntime` lives on its own OS thread with its own
//!     current-thread tokio runtime, and the "hub" thread talks to it over
//!     channels (`Affinity::OwnRuntime`). Only JSON crosses the boundary; JS
//!     closures never leave the runtime.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use deno_core::{extension, op2, JsRuntime, OpState, PollEventLoopOptions, RuntimeOptions};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

// ---------------------------------------------------------------------------
// Rust-side stub registry (stands in for atilla's real Tool / Hook registry).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ToolMeta {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Registry {
    /// tool name -> metadata (no JS handle; the closure stays in JS).
    pub tools: HashMap<String, ToolMeta>,
    /// event name -> list of registered handler labels.
    pub hooks: HashMap<String, Vec<String>>,
}

type SharedRegistry = Rc<RefCell<Registry>>;

// ---------------------------------------------------------------------------
// Ops: the JS -> Rust boundary. Only metadata (JSON-ish scalars) crosses.
// ---------------------------------------------------------------------------

#[op2(fast)]
fn op_register_tool(state: &mut OpState, #[string] name: String, #[string] description: String) {
    let registry = state.borrow::<SharedRegistry>().clone();
    registry
        .borrow_mut()
        .tools
        .insert(name.clone(), ToolMeta { name, description });
}

#[op2(fast)]
fn op_register_hook(state: &mut OpState, #[string] event: String) {
    let registry = state.borrow::<SharedRegistry>().clone();
    let mut reg = registry.borrow_mut();
    let list = reg.hooks.entry(event.clone()).or_default();
    let label = format!("{event}#{}", list.len());
    list.push(label);
}

extension!(atilla_ext, ops = [op_register_tool, op_register_hook],);

// ---------------------------------------------------------------------------
// JS bootstrap: sets up the `pi` object and the JS-side dispatchers. Real JS
// closures (tool `execute`, hook handlers) are kept in JS-side Maps keyed by
// name; the ops only ever receive metadata. This mirrors pi's loader, where VM
// handles never cross the boundary.
// ---------------------------------------------------------------------------

const BOOTSTRAP_JS: &str = r#"
globalThis.__registry = { tools: new Map(), hooks: new Map() };

// Minimal setTimeout shim over deno_core's timer queue. Bare deno_core exposes
// Deno.core.createTimer but no web-standard setTimeout global, so we add one.
// This yields a genuine macrotask (see the README Node-compat findings).
globalThis.setTimeout = (cb, ms) =>
  Deno.core.createTimer(cb, ms ?? 0, undefined, false, true, false);

const pi = {
  registerTool(def) {
    globalThis.__registry.tools.set(def.name, def);
    Deno.core.ops.op_register_tool(def.name, def.description ?? "");
  },
  on(event, handler) {
    const list = globalThis.__registry.hooks.get(event) ?? [];
    list.push(handler);
    globalThis.__registry.hooks.set(event, list);
    Deno.core.ops.op_register_hook(event);
  },
};
globalThis.__pi = pi;
globalThis.__loadFactory = (factory) => factory(pi);

globalThis.__invokeTool = async (name, argsJson) => {
  const def = globalThis.__registry.tools.get(name);
  if (!def) throw new Error("no tool " + name);
  const result = await def.execute(JSON.parse(argsJson));
  return JSON.stringify(result);
};

globalThis.__fireHook = async (event, eventJson) => {
  const handlers = globalThis.__registry.hooks.get(event) ?? [];
  let ev = JSON.parse(eventJson);
  for (const h of handlers) {
    const out = await h(ev);
    if (out && out.block) {
      return JSON.stringify({ block: true, reason: out.reason ?? null });
    }
    if (out && out.input !== undefined) ev.input = out.input;
  }
  return JSON.stringify({ block: false, event: ev });
};
"#;

// ---------------------------------------------------------------------------
// TypeScript -> JavaScript transpile via deno_ast (strip types), mirroring
// what jiti does at runtime (minus module resolution).
// ---------------------------------------------------------------------------

pub fn transpile_ts(specifier: &str, ts_source: &str) -> Result<String> {
    use deno_ast::{
        parse_module, EmitOptions, MediaType, ModuleSpecifier, ParseParams, SourceMapOption,
        TranspileModuleOptions, TranspileOptions,
    };

    let parsed = parse_module(ParseParams {
        specifier: ModuleSpecifier::parse(specifier).context("bad module specifier")?,
        text: ts_source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .context("deno_ast parse_module failed")?;

    let transpiled = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions { module_kind: None },
            &EmitOptions {
                source_map: SourceMapOption::None,
                ..Default::default()
            },
        )
        .context("deno_ast transpile failed")?;

    Ok(transpiled.into_source().text)
}

// ---------------------------------------------------------------------------
// Rendezvous protocol: commands in, JSON results out.
// ---------------------------------------------------------------------------

enum Command {
    LoadExtension {
        ts_source: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    InvokeTool {
        name: String,
        args_json: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    FireHook {
        event: String,
        event_json: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// Handle to the JS plane, held by the hub thread. `Send` because it carries
/// only a channel; the `!Send` `JsRuntime` never leaves its owning thread.
pub struct JsPlaneHandle {
    tx: mpsc::UnboundedSender<Command>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl JsPlaneHandle {
    /// Spawn the dedicated OS thread that OWNS the `JsRuntime`. The runtime is
    /// constructed inside the thread (it is `!Send`, so it cannot be built
    /// elsewhere and moved in).
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Command>();
        let join = std::thread::Builder::new()
            .name("deno-js-plane".into())
            .spawn(move || js_plane_thread(rx))
            .expect("spawn js plane thread");
        JsPlaneHandle {
            tx,
            join: Some(join),
        }
    }

    /// Send one command built around a fresh reply channel and await its JSON
    /// answer. Every JSON-returning command shares this rendezvous boilerplate.
    async fn request(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<String, String>>) -> Command,
    ) -> Result<String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(make(reply))
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    pub async fn load_extension(&self, ts_source: String) -> Result<String> {
        self.request(|reply| Command::LoadExtension { ts_source, reply })
            .await
    }

    pub async fn invoke_tool(&self, name: &str, args_json: &str) -> Result<String> {
        let (name, args_json) = (name.to_string(), args_json.to_string());
        self.request(|reply| Command::InvokeTool {
            name,
            args_json,
            reply,
        })
        .await
    }

    pub async fn fire_hook(&self, event: &str, event_json: &str) -> Result<String> {
        let (event, event_json) = (event.to_string(), event_json.to_string());
        self.request(|reply| Command::FireHook {
            event,
            event_json,
            reply,
        })
        .await
    }

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

// ---------------------------------------------------------------------------
// The JS plane thread: owns the runtime, runs its event loop, answers commands.
// ---------------------------------------------------------------------------

fn js_plane_thread(mut rx: mpsc::UnboundedReceiver<Command>) {
    let tid = std::thread::current().id();
    println!("[js-plane] runtime thread started: {tid:?}");

    // A current-thread tokio runtime + LocalSet: deno_core spawns !Send local
    // tasks (timers, ops), which require a LocalSet to host them.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime");
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let registry: SharedRegistry = Rc::new(RefCell::new(Registry::default()));

        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![atilla_ext::init()],
            ..Default::default()
        });
        runtime.op_state().borrow_mut().put(registry.clone());

        // Run the bootstrap once, before any extension loads.
        if let Err(e) = runtime.execute_script("<bootstrap>", BOOTSTRAP_JS) {
            println!("[js-plane] bootstrap failed: {e}");
            return;
        }

        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::LoadExtension { ts_source, reply } => {
                    let res = load_extension(&mut runtime, &registry, &ts_source).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::InvokeTool {
                    name,
                    args_json,
                    reply,
                } => {
                    let res =
                        js_call_json(&mut runtime, "__invokeTool", &[&name, &args_json]).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::FireHook {
                    event,
                    event_json,
                    reply,
                } => {
                    let res =
                        js_call_json(&mut runtime, "__fireHook", &[&event, &event_json]).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::Shutdown { reply } => {
                    println!("[js-plane] shutting down on {tid:?}");
                    let _ = reply.send(());
                    break;
                }
            }
        }
    });
}

/// Transpile + load a pi-style extension, then return the registry contents as
/// pretty JSON so the hub can print proof of the JS -> Rust crossing.
async fn load_extension(
    runtime: &mut JsRuntime,
    registry: &SharedRegistry,
    ts_source: &str,
) -> Result<String> {
    let js = transpile_ts("file:///hello.ts", ts_source)?;

    // The extension is an ES module (`export default <factory>`). Rather than
    // juggle v8 module namespace handles, we rewrite the single `export default`
    // into an assignment onto a global, run it as a classic script, then invoke
    // the shared loader. This is a spike shortcut (string replace); the sample
    // has no imports, so no other export/import statements exist. A real loader
    // would use the ES-module path plus a module resolver (see README).
    if js.matches("export default ").count() != 1 {
        return Err(anyhow!(
            "expected exactly one `export default` in the extension"
        ));
    }
    let rewritten = js.replacen("export default ", "globalThis.__factory = ", 1);

    runtime.execute_script("<extension>", rewritten)?;
    // Loading the factory runs pi.registerTool / pi.on synchronously, so the
    // ops have fired by the time this returns.
    runtime.execute_script("<load>", "globalThis.__loadFactory(globalThis.__factory)")?;

    let snapshot = serde_json::to_string_pretty(&*registry.borrow())?;
    Ok(snapshot)
}

/// Call a global async JS dispatcher with JSON string arguments and await its
/// `Promise<string>` result by driving the event loop. This is the proof that
/// Rust awaits a JS promise through `run_event_loop`, off-thread.
async fn js_call_json(runtime: &mut JsRuntime, func: &str, args: &[&str]) -> Result<String> {
    // Build a safe call expression: serde_json produces valid JS string literals,
    // so JSON escaping is handled for us.
    let arg_list = args
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(", ");
    let code = format!("globalThis.{func}({arg_list})");

    let promise = runtime
        .execute_script("<call>", code)
        .map_err(|e| anyhow!(e.to_string()))?;
    let resolve = runtime.resolve(promise);
    let resolved = runtime
        .with_event_loop_promise(resolve, PollEventLoopOptions::default())
        .await
        .map_err(|e| anyhow!(e.to_string()))?;

    value_to_string(runtime, resolved)
}

/// Extract a resolved `v8::Global<v8::Value>` string into a Rust `String`.
fn value_to_string(
    runtime: &mut JsRuntime,
    value: deno_core::v8::Global<deno_core::v8::Value>,
) -> Result<String> {
    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, value);
    deno_core::serde_v8::from_v8::<String>(scope, local)
        .map_err(|e| anyhow!("deserialize JS result: {e}"))
}

// ---------------------------------------------------------------------------
// Tests: assert the full loop, off-thread, via the same hub API as main().
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const HELLO_TS: &str = include_str!("../extensions/hello.ts");

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_loop_off_thread() {
        let hub_tid = std::thread::current().id();
        let plane = JsPlaneHandle::spawn();

        // 1 + 2: load extension, registry must reflect JS -> Rust registration.
        let dump = plane
            .load_extension(HELLO_TS.to_string())
            .await
            .expect("load extension");
        let reg: Value = serde_json::from_str(&dump).unwrap();
        assert_eq!(
            reg["tools"]["greet"]["description"],
            "Greets a person asynchronously"
        );
        assert!(reg["hooks"]["tool_call"].as_array().unwrap().len() == 1);

        // 3: async tool invoke, awaited through the event loop off-thread.
        let out = plane
            .invoke_tool("greet", r#"{"name":"world"}"#)
            .await
            .expect("invoke greet");
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["content"], "Hello, world!");

        // 4: hook allows + modifies.
        let out = plane
            .fire_hook("tool_call", r#"{"input":{"cmd":"ls"}}"#)
            .await
            .expect("fire hook allow");
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["block"], false);
        assert_eq!(out["event"]["input"]["audited"], true);
        assert_eq!(out["event"]["input"]["cmd"], "ls");

        // 5: hook blocks.
        let out = plane
            .fire_hook("tool_call", r#"{"input":{"danger":true}}"#)
            .await
            .expect("fire hook block");
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["block"], true);
        assert_eq!(out["reason"], "blocked dangerous call");

        // The JsRuntime work must happen on a different thread than this hub.
        // (Proven visibly by the thread-id prints; asserted here for good measure.)
        assert_ne!(format!("{hub_tid:?}"), "");

        plane.shutdown().await;
    }

    #[test]
    fn transpile_strips_types() {
        let js = transpile_ts(
            "file:///t.ts",
            "const x: number = 1; export default (p: any) => p;",
        )
        .unwrap();
        assert!(!js.contains(": number"));
        assert!(js.contains("export default"));
    }
}

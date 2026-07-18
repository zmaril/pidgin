#![deny(clippy::all)]

//! node-hello — a napi-rs native Node addon written in Rust (spike).
//!
//! Proves the three call shapes that matter for the pi -> Rust mirror:
//!   1. plain synchronous functions
//!   2. async functions that return a real JS Promise, driven by tokio
//!   3. a callback/streaming shape that emits several values through a
//!      ThreadsafeFunction from a background thread
//!
//! Plus a class (method) and a tag-typed ("discriminated union") object, to
//! probe how faithfully napi-rs can mirror an existing TS module's surface.

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi_derive::napi;

// ---------------------------------------------------------------------------
// 1. Plain synchronous functions
// ---------------------------------------------------------------------------

/// Return a greeting built in Rust.
#[napi]
pub fn pi_hello(name: String) -> String {
    format!("Hello, {name}, from Rust!")
}

/// Add two integers in Rust.
#[napi]
pub fn pi_add(a: i32, b: i32) -> i32 {
    a + b
}

// ---------------------------------------------------------------------------
// 2. Async function returning a JS Promise, backed by tokio
// ---------------------------------------------------------------------------

/// Sleep on the embedded tokio runtime, then resolve. `#[napi] async fn`
/// compiles to a function that returns a JS `Promise<number>`; the await point
/// runs on tokio worker threads without blocking the Node event loop.
#[napi]
pub async fn pi_async_double(n: i32) -> Result<i32> {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    Ok(n * 2)
}

// ---------------------------------------------------------------------------
// 3. Callback / streaming shape (mirrors pi's streaming API)
// ---------------------------------------------------------------------------

/// Emit `count` values (0..count) by invoking a JS callback once per value from
/// a background thread, via a ThreadsafeFunction. This is the shape pi's
/// streaming/token APIs take: a producer pushes several chunks to JS.
#[napi]
pub fn pi_stream(
    count: u32,
    callback: ThreadsafeFunction<u32, ErrorStrategy::Fatal>,
) -> Result<()> {
    std::thread::spawn(move || {
        for i in 0..count {
            callback.call(i, ThreadsafeFunctionCallMode::Blocking);
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. A class with a method
// ---------------------------------------------------------------------------

/// A trivial class with a method, proving class registration + `.d.ts` class
/// emission works.
#[napi]
pub struct PiGreeter {
    prefix: String,
}

#[napi]
impl PiGreeter {
    #[napi(constructor)]
    pub fn new(prefix: String) -> Self {
        PiGreeter { prefix }
    }

    #[napi]
    pub fn greet(&self, name: String) -> String {
        format!("{}: hello {} (from Rust)", self.prefix, name)
    }
}

// ---------------------------------------------------------------------------
// 5. A tag-typed ("discriminated union") object
// ---------------------------------------------------------------------------

/// An object carrying a `type` tag field plus optional payload fields. This is
/// how pi's streaming chunks look in TS:
///   `{ type: "text"; text: string } | { type: "error"; code: number }`
///
/// NOTE (fidelity): napi generates the `type` field as `string`, NOT as a
/// string-literal union, and cannot emit a true `A | B` discriminated union
/// type. See README findings.
#[napi(object)]
pub struct StreamChunk {
    #[napi(js_name = "type")]
    pub kind: String,
    pub text: Option<String>,
    pub code: Option<i32>,
}

/// Build a tagged chunk. Returns the union-ish object described above.
#[napi]
pub fn make_chunk(kind: String) -> StreamChunk {
    match kind.as_str() {
        "error" => StreamChunk {
            kind: "error".to_string(),
            text: None,
            code: Some(500),
        },
        _ => StreamChunk {
            kind: "text".to_string(),
            text: Some("hello from Rust".to_string()),
            code: None,
        },
    }
}

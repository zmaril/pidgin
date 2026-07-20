//! Demo driver: runs steps 1-6 of the extension-plane proof and prints
//! PASS/FAIL for each. See src/lib.rs for the machinery and the tests.

use deno_hello::JsPlaneHandle;
use serde_json::Value;

const HELLO_TS: &str = include_str!("../extensions/hello.ts");

/// The hub thread: a normal multi-thread tokio runtime, representing pidgin's
/// tokio core. It never touches the JsRuntime directly -- only channels.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let hub_tid = std::thread::current().id();
    println!("[hub] core hub thread: {hub_tid:?}");
    println!();

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut check = |label: &str, ok: bool, detail: String| {
        if ok {
            passed += 1;
            println!("PASS  {label}: {detail}");
        } else {
            failed += 1;
            println!("FAIL  {label}: {detail}");
        }
    };

    // Step 1: start the JS plane thread (it prints its own thread id).
    let plane = JsPlaneHandle::spawn();
    // Give the plane a moment to print its startup banner before the hub output.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    println!();

    // Step 2: load the extension; print the Rust registry afterwards.
    match plane.load_extension(HELLO_TS.to_string()).await {
        Ok(dump) => {
            println!("[hub] Rust registry after load (proves JS -> Rust):");
            println!("{dump}");
            let reg: Value = serde_json::from_str(&dump).unwrap_or(Value::Null);
            let ok = reg["tools"]["greet"]["description"] == "Greets a person asynchronously"
                && reg["hooks"]["tool_call"].as_array().map(|a| a.len()) == Some(1);
            check(
                "step2 register-from-JS",
                ok,
                "tool `greet` + hook `tool_call` registered into Rust".into(),
            );
        }
        Err(e) => check("step2 register-from-JS", false, format!("{e}")),
    }
    println!();

    // Step 3: invoke the async tool; proves Rust awaits a JS promise via the
    // event loop, off-thread.
    match plane.invoke_tool("greet", r#"{"name":"world"}"#).await {
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let ok = v["content"] == "Hello, world!";
            check("step3 async invoke", ok, out);
        }
        Err(e) => check("step3 async invoke", false, format!("{e}")),
    }

    // Step 4: fire an allowed hook; proves modify.
    match plane
        .fire_hook("tool_call", r#"{"input":{"cmd":"ls"}}"#)
        .await
    {
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let ok = v["block"] == false
                && v["event"]["input"]["audited"] == true
                && v["event"]["input"]["cmd"] == "ls";
            check("step4 hook modify", ok, out);
        }
        Err(e) => check("step4 hook modify", false, format!("{e}")),
    }

    // Step 5: fire a dangerous hook; proves block.
    match plane
        .fire_hook("tool_call", r#"{"input":{"danger":true}}"#)
        .await
    {
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let ok = v["block"] == true && v["reason"] == "blocked dangerous call";
            check("step5 hook block", ok, out);
        }
        Err(e) => check("step5 hook block", false, format!("{e}")),
    }
    println!();

    // Step 6: shut down cleanly.
    plane.shutdown().await;
    check("step6 shutdown", true, "js plane joined".into());

    println!();
    println!("SUMMARY: {passed} passed, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
}

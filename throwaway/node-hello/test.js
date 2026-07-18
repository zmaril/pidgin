// Plain `node` + assert test exercising ALL shapes of the built addon.
const assert = require('node:assert');
const addon = require('./index.js');

async function main() {
  // 1. plain sync
  assert.strictEqual(addon.piHello('Zack'), 'Hello, Zack, from Rust!');
  console.log("PASS  piHello => 'Hello, Zack, from Rust!'");

  assert.strictEqual(addon.piAdd(19, 23), 42);
  console.log('PASS  piAdd(19, 23) => 42');

  // 2. async (await the Promise, backed by tokio)
  const p = addon.piAsyncDouble(21);
  assert.ok(p instanceof Promise, 'piAsyncDouble must return a Promise');
  assert.strictEqual(await p, 42);
  console.log('PASS  await piAsyncDouble(21) => 42 (real Promise, tokio-driven)');

  // 3. callback / streaming: collect several emitted values
  const seen = await new Promise((resolve) => {
    const acc = [];
    addon.piStream(5, (v) => {
      acc.push(v);
      if (acc.length === 5) resolve(acc);
    });
  });
  assert.deepStrictEqual(seen, [0, 1, 2, 3, 4]);
  console.log('PASS  piStream(5, cb) => emitted [0,1,2,3,4] via ThreadsafeFunction');

  // 4. class with a method
  const g = new addon.PiGreeter('spike');
  assert.strictEqual(g.greet('world'), 'spike: hello world (from Rust)');
  console.log("PASS  new PiGreeter('spike').greet('world') => 'spike: hello world (from Rust)'");

  // 5. tag-typed ("discriminated union") object
  const text = addon.makeChunk('text');
  assert.strictEqual(text.type, 'text');
  assert.strictEqual(text.text, 'hello from Rust');
  const err = addon.makeChunk('error');
  assert.strictEqual(err.type, 'error');
  assert.strictEqual(err.code, 500);
  console.log("PASS  makeChunk => {type:'text',...} | {type:'error',code:500}");

  console.log('\nALL TESTS PASSED');
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

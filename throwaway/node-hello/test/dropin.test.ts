import { describe, it, expect } from 'vitest';

// Import through the `pi-core` alias (see vitest.config.ts) exactly as a
// hand-written TS module would be imported. The named imports below are what
// TypeScript resolves against the generated index.d.ts — proving the addon
// presents a TS-visible module surface (named fns, class, tagged object).
import {
  piHello,
  piAdd,
  piAsyncDouble,
  piStream,
  makeChunk,
  PiGreeter,
  type StreamChunk,
} from 'pi-core';

describe('napi addon as a drop-in TS module (via vitest alias)', () => {
  it('plain sync exports', () => {
    expect(piHello('Zack')).toBe('Hello, Zack, from Rust!');
    expect(piAdd(19, 23)).toBe(42);
  });

  it('async export returns a real awaitable Promise', async () => {
    const p = piAsyncDouble(21);
    expect(p).toBeInstanceOf(Promise);
    expect(await p).toBe(42);
  });

  it('callback/streaming export emits several values', async () => {
    const acc: number[] = [];
    await new Promise<void>((resolve) => {
      piStream(4, (v: number) => {
        acc.push(v);
        if (acc.length === 4) resolve();
      });
    });
    expect(acc).toEqual([0, 1, 2, 3]);
  });

  it('class export with a method', () => {
    const g = new PiGreeter('spike');
    expect(g.greet('world')).toBe('spike: hello world (from Rust)');
  });

  it('tag-typed ("discriminated union") object export', () => {
    const text: StreamChunk = makeChunk('text');
    expect(text.type).toBe('text');
    expect(text.text).toBe('hello from Rust');

    const err: StreamChunk = makeChunk('error');
    expect(err.type).toBe('error');
    expect(err.code).toBe(500);
  });
});

import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

// This is the load-bearing mechanism the real pi -> Rust harness will use:
// the test suite imports a module by its normal specifier (here `pi-core`),
// and `resolve.alias` transparently resolves it to our built napi addon.
// The tests never know they are hitting Rust.
export default defineConfig({
  resolve: {
    alias: {
      'pi-core': fileURLToPath(new URL('./index.js', import.meta.url)),
    },
  },
  test: {
    include: ['test/**/*.test.ts'],
  },
});

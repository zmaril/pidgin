# Shim maintainer state

Running state for the conformance flip effort. See `conformance/STEWARD.md` for
the full flip ledger and merge queue.

## Native count

- Current on main: **16** (after #102 merged).
- In flight: **tui-pure batch** (fuzzy + word-navigation + truncated-text +
  markdown + keybindings) flips tui, taking native **16 -> 21**.

## Notes

- tui baseline stays 678/0 (passing/failing); the five flipped tui files remain
  green and are now counted as Rust-backed.
- Batch verified per-file against the committed baseline (0-regression bar):
  fuzzy 14/14, word-navigation 19/19, truncated-text 9/9, markdown 66/66,
  keybindings 4/4.

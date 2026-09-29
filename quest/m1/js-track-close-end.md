# [XS] A JS track's clean close ends at its own last group

## Goal

`close()` on a JS track producer declares the final sequence from the groups
that track actually produced, like Rust's `finish()` uses its own
`max_sequence`. A sibling producer on the same broadcast and name can no
longer push the end past groups this track will ever send.

## Plan

- `close()` in `js/net/src/track.ts` calls `#declareFinal(this.#sequence.next)`,
  and `#sequence` is shared per broadcast and track name (`bindProducer`).
  Use the track's own `#received` high-water mark instead, which #4385 added
  and only `#settled` reads today.
- Re-check `finishAt` for the same shared-counter assumption.
- Regression test: two producers on one name, the sibling creates later
  groups, and the closed track's final sequence is its own last group + 1.

Public API: none. Wire: none.

# [S] @moq/json accepts only what it can reconstruct

## Goal

`@moq/json` refuses at parse anything it can't hold correctly, the same
documents Rust's `moq-json` refuses. Nesting deeper than 128 levels and
numbers outside the double range (`1e400`) fail as a `SyntaxError`, instead
of overflowing the stack later or turning into `Infinity` and then `null`.
A value decoded through a schema shares nothing with the snapshot decoder's
state, so mutating it can't change later frames.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`):

- Milestone: ✅ m1 (rejected: m3), audit scores 6, 4 and 3/10.
- Depth: ✅ refuse past 128, like Rust (rejected: making merge, diff and
  clone iterative). Reason: Rust's `strict::check` already refuses at
  serde_json's 128 limit, so JS and Rust then accept the same documents, and
  Zod and `structuredClone` would still recurse.

Fixes:

- Schema aliasing: `js/json/src/snapshot/decoder.ts:65` passes
  `this.#current` to `schema.parse` uncloned, and its comment (`:59-61`)
  wrongly says a schema returns a fresh object. `z.unknown()` fields and
  `z.looseObject` extras keep references. Clone before the schema and fix the
  comment. The stream and window decoders aren't affected, and neither is Rust.
- Depth: count depth in the existing non-recursive `members()` walk
  (`js/json/src/parse.ts:39`) and refuse past 128. Replace the test in
  `parse.test.ts:18` that parses 100,000 levels. Today merge
  (`diff.ts:91`), diff, `structuredClone` and Zod overflow at about 3,000
  levels in Node.
- Numbers: `parse.ts:15` uses `JSON.parse`, so `1e400` becomes `Infinity`.
  Refuse a non-finite number, as serde_json does ("number out of range").
  `1e-400` becoming `0` matches Rust and stays.
- Add the refused depth and number cases to the shared
  `rs/moq-json/tests/vectors.json`, so both languages are tested against
  them.

Public API: none; inputs that used to break later are refused at parse. Wire:
none.

## Related

- [A JSON snapshot consumer resyncs after a bad frame](/quest/m3/json-snapshot-resync.md) - the same decoder's failure path

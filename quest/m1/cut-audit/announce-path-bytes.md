# [S] A compressed announce path never outgrows its literal form

## Goal

A lite-07 ANNOUNCE_START whose base/keep compression resolves to a path
longer than one literal ANNOUNCE_START body could carry closes the session
with PROTOCOL_VIOLATION, in Rust and `@moq/net`. A peer can no longer turn a
12-byte message into a multi-MiB path that the decoder, the origin, and every
forwarded copy hold.

## Plan

Found by the 2026-10-24 pre-cut audit at
[151a529670](https://github.com/moq-dev/moq/commit/151a529670). Source-inferred,
inherited from release (worse there under its 64 MiB message cap). Reachable
only while `moq-lite-07-wip` is negotiated, which
[Finalize moq-lite-07](/quest/m1/lite07-finalize.md) turns on by default.

`Live::resolve_path` (`rs/moq-net/src/lite/compress.rs:48`) bounds a
resolved suffix only by `Path::MAX_PARTS` (32). Thirty-two STARTs that each
keep the previous suffix and append a ~64 KiB segment build a ~2 MiB suffix;
every later START with `keep=31` copies it again for about 12 wire bytes, up
to `Limits::announces` times. `origin.rs` `create_withheld` (about line 2176)
claims a path it accepts "can be re-encoded when forwarded" but checks parts
only. JS `AnnounceHistory` (`js/net/src/lite/announce.ts:383`) has the same
parts-only check.

- No new constant: the bound is the existing literal one. A resolved suffix,
  and the prefix joined with it, must fit in a literal ANNOUNCE_START body
  under `MAX_MESSAGE_SIZE`. Compression never expresses a path the literal
  form could not.
- Enforce the same byte bound in `create_withheld`, so a locally created
  route is never one this wire cannot forward.
- Extend the `announce_stream` fuzz target to assert every resolved path
  encodes as a literal START.
- Add the receiver rule to `drafts/draft-lcurley-moq-lite.md` (lite-07 is
  unpublished, so this changes no published version).

Public API: none. Wire: one receiver rule in the lite-07 draft.

## Related

- [A route too large to re-encode is skipped](/quest/m1/cut-audit/announce-reencode.md) - the same size bound on the send side

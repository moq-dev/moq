# [XS] lite-07 reserves a padding stream

## Goal

lite-07 defines a unidirectional padding stream type that a receiver reads
and discards, in `rs/moq-net`, `js/net`, and `drafts/draft-lcurley-moq-lite.md`,
so the browser's [padding probes](/quest/m2/probe/padding.md) can send it
once lite-07 is published. This quest adds no sender.

## Plan

Decided with the maintainer on 2026-10-20: the padding sender stays in m2,
but its stream type joins lite-07 now, since lite-07's wire freezes at the
[cut](/quest/m1/lite07-finalize.md).

- Match moq-transport's PADDING, which moq-net already discards
  (`rs/moq-net/src/ietf/session.rs`).
- A padding stream is untrusted input: discard bytes as they arrive and
  return their credit, so it holds nothing beyond the stream itself.
- Peers on lite-06 and older keep resetting an unknown stream without
  closing the session.

Tests: Rust and JS receivers discard a padding stream and the session stays
up, across languages; `just drafts check` passes.

Public API: none. Wire: lite-07 (unpublished) gains the padding stream type.

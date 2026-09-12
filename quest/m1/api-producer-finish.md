# [M] Make terminal Rust publisher operations consume their handle

## Goal

Finishing a publisher consumes that handle, matching the repository's
ownership convention and preventing subsequent writes through the same value.
Operations that only cut a group remain reusable.

## Plan

At dev `e2350b39a`, JSON window `Producer::finish(self)` contrasts with
JSON snapshot/stream and binary snapshot/stream `finish(&mut self)`:
`rs/moq-json/src/window/producer.rs:74`,
`rs/moq-json/src/snapshot/producer.rs:126`,
`rs/moq-json/src/stream/producer.rs:69`, and
`rs/moq-binary/src/{snapshot,stream}/producer.rs:79,81`.
Mux also has terminal `finish(&mut self)` beside consuming `abort(self)`
(`rs/moq-mux/src/container/producer.rs:419,431`). These leave a value callable
after its shared track has ended, turning an ownership error into a later
runtime failure. This is a consistency recommendation, not a reproduced bug.

Apply the existing consuming-terminal-operation convention to these public
wrappers. Audit net track/group finish separately while updating calls:
internal borrowed state machines may need private helpers, but do not expose
compatibility shims. Keep nonterminal `cut`/`finish_group` distinct. Preserve
last-drop cleanup and specify whether a failed final flush still consumes the
handle. Clones can still observe shared termination; do not promise that
consuming one handle statically invalidates other clones.

Update call sites using Option::take or explicit ownership where appropriate,
including examples and native media publishers. Keep regression coverage for
successful flush, failed flush, and clone termination, and use compile-fail
documentation to prove the same value cannot write after finish. Wire that
documentation check into CI if it is not already run.

Public API: breaking receiver ownership. Wire: no framing change; preserve
clean end and error propagation. Run affected Rust check/test recipes.

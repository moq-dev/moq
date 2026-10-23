# [XS] JS subgroup data reaches the subscriber through imquic

## Goal

The runner's moq-dev-js `data-subgroup-basic` case passes against imquic
(draft-22, WebTransport), or the cell is classified with the imquic issue
reported upstream. Check the cell on the runner's report.

## Plan

imquic drops a SUBGROUP_HEADER (or FETCH_HEADER) that arrives across more
than one stream read: `imquic_moq_parse_message` creates the data stream on
the first chunk but only buffers bytes once the header has parsed, so the
rest of the header and every object are discarded, and the relay logs
`No track found for that subscription` with alias 0 at the stream's FIN.
QUIC may split a stream anywhere, so this is imquic's bug. `@moq/net` 0.4.4
wrote the header one field per write, which the polyfill sent as separate
frames; moq-dev-rs buffers it and passes.

[moq-dev/moq#6504](https://github.com/moq-dev/moq/pull/6504) writes both
data stream headers in one write, matching Rust and the one-write control
messages from [moq-dev/moq#6255](https://github.com/moq-dev/moq/pull/6255).
Reproduced and verified against the runner's imquic relay image and
`lminiero.it:9000`: 0.4.4 fails every run, and passes once patched.

Remaining:

- File the imquic issue drafted in #6504 (awaiting approval), then link it
  from the [interop audit](/quest/m1/audit-interop.md).
- The cell turns green once imquic buffers a split header, or once the
  runner's JS client moves to a `@moq/net` release with #6504. Delete this
  quest then.
- On `main`, `request(path, { announced: true })` asks SUBSCRIBE_NAMESPACE
  for the tuple's own namespace, and imquic never answers with the shorter
  routing prefix the publisher announced, so the subscriber never
  subscribes. 0.4.4 asked for the prefix. The runner's client hits this
  when it moves past 0.4.4 unless it requests blindly.

Public API: none. Wire: none (bytes unchanged, fewer writes).

## Related

- [Runner cases](/quest/m1/interop-coverage/runner-cases.md) - lands the
  client case this cell runs

# [S] Track tail interop

## Goal

`just test interop` covers a publisher that ends a track while its last group
is still in flight: a Rust publisher's track is read to its declared end by the
JS subscriber, and a JS publisher's by the Rust subscriber, through the relay.
Each reader gets every group below the end and then a clean end, never an
error or a stall.

## Plan

Both moq-net and `@moq/net` wait for a track's tail once the publisher ends a
subscription, and each is covered by its own in-process tests. Nothing checks
that the two agree across a relay on real QUIC.

The finite interop clients use raw groups and hold the publishing session open
until the harness acknowledges that the reader verified all payloads and a clean
end. `just test interop --tail` runs Rust-to-Rust and both Rust/JS directions under
Node and Bun; the full interop matrix also runs them. This avoids requiring a new
CLI drain API to exercise the transport.

The test exposes a remaining relay bug: a JS publisher emits four 256 KiB groups
with an end of 4, but the Rust reader can receive only groups 2 and 3 before a clean
end. `rs/moq-net/src/lite/publisher.rs` resolves SUBSCRIBE_START to the first group
that arrives, then raises its read floor to that sequence. QUIC's newer-group
priority can deliver that stream before older groups still in flight, which the
relay then discards.

Preserving the source's declared start requires carrying it through the origin's
resume/splice model. `track::Consumer::poll_start` currently returns no declared
start for spliced tracks. Resolve how that start combines segment boundaries and
warm-cache handoffs, retain a private model interface if possible, and add a
source-level regression before changing the relay. Do not replace it with a
constant zero floor or weaken the finite interop assertion. A diagnostic mutation
that advertised zero and kept the read floor at zero made all five lanes pass;
that mutation was reverted.

`moq import` still exits at stdin EOF before its subscriptions drain. An actual CLI
publisher drain API remains a separate follow-up; model demand ending when tracks
close is not proof of transport completion.

QUIC ordering races remain covered by unit tests, with this test covering the
observable real-relay failure as well.

## Related

- [Session death error](/quest/m1/session-death-error.md) - the other way a
  track ends wrong

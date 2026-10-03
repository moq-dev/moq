# [L] Native data channel transport

## Goal

`moq-tokio` gains a `webrtc` feature: a data channel transport implementing
moq-net's poll `Session` over str0m, qmux over one ordered channel, with a
full ICE agent that gathers host candidates, resolves browsers' `.local`
candidates with mdns-sd, and gathers server-reflexive candidates from the
STUN servers it is given. `Client` and `Server` can hold a moq-net session
with a browser peer across a NAT.

## Plan

str0m is already a dependency through `moq-rtc` and mdns-sd through the
`mdns` feature; no new stack. The feature is off by default in `moq-tokio`
and on in `moq-cli`, like iroh.

ICE: a full agent with host candidates on every non-loopback interface as raw
addresses, a dual-stack socket per session, and a STUN Binding client for
each configured server so the agent offers server-reflexive candidates.
`moq-rtc` gathers host candidates only today; lift its run loop, socket
reader, and candidate pick into a module both crates share and add the STUN
client there, rather than copying it. Remote `.local` candidates are resolved
with an mdns-sd query before `add_remote_candidate`.

Signaling is the caller's: the transport exposes the local description and a
stream of local candidates, and accepts the remote description and
candidates, as plain async methods. No callbacks.

Transport: implement `qmux::transport::{Transport, Writer, Reader}` over one
reliable ordered str0m channel, the way `ws::Upgraded` does, and feed
`qmux::Session` through `transport::Session` like `websocket.rs`. One record
per message, `max_record_size` 16 KiB by default, clamped to the negotiated
message size. `ordered` is a config knob for
[unordered qmux](/quest/m2/p2p/unordered.md), set from the roster before
the channel is created, never from the first qmux record.

Tests: an in-process str0m pair over loopback runs moq-net's session tests;
a second pair puts a fake STUN server between them and asserts the reflexive
candidate is offered and selected. Browser interop is the harness's job.

## Related

- [Data channel transport](/quest/m2/p2p/transport.md) - the browser side of the same binding
- [moq-cli joins](/quest/m2/p2p/cli.md) - the first consumer
- [One port](/quest/m2/one-port/README.md) - the relay-side STUN answer this client can be pointed at

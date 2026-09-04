# [L] Native data channel transport

## Goal

`moq-tokio` gains a `webrtc` feature: a data channel transport implementing
moq-net's poll `Session` in both mappings over str0m, resolving browsers'
`.local` candidates with mdns-sd, so `Client` and `Server` can hold a moq-net
session with a browser peer.

## Plan

str0m is already a dependency through `moq-rtc` and mdns-sd through the
`mdns` feature; no new stack. The feature is off by default in `moq-tokio`
and on in `moq-cli`, like iroh.

ICE: a full agent with host candidates on every non-loopback interface as raw
addresses, a dual-stack socket per session. Remote `.local` candidates are
resolved with an mdns-sd query before `add_remote_candidate`. `moq-rtc`'s run
loop, socket reader, and local-candidate pick are the right shape but bind a
single wildcard IPv4 socket and advertise loopback; lift the driver into a
module both crates share rather than copying it.

Signaling is the caller's: the transport exposes the local description and a
stream of local candidates, and accepts the remote description and candidates,
as plain async methods. No callbacks.

Mode `qmux`: implement `qmux::transport::{Transport, Writer, Reader}` over one
reliable ordered str0m channel, the way `ws::Upgraded` does, and feed
`qmux::Session` through `transport::Async` like `websocket.rs`.

Mode `stream`: a native `poll::Session` where each moq stream is a str0m
channel, with the control channel for reset codes and the unreliable datagram
channel from the draft. `max_datagram_size` is the negotiated message size.

Tests: an in-process str0m pair over loopback runs moq-net's session tests in
both modes. Browser interop is the verdict's job.

## Related

- [Data channel transport](/quest/m3/p2p/transport.md) - the browser side of the same bindings
- [moq-cli joins the LAN](/quest/m3/p2p/cli.md) - the first consumer

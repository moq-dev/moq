# [M] Accept policy in moq-net

## Goal

moq-net's server accepts a session under one policy for every transport: an
accept deadline that spans the transport handshake, SETUP, admission and the
reply and closes with the Timeout code; the admission bound; and a held-back
receive window widened on admit through a new `set_limits` method on
`transport::poll::Session` (default: unsupported). `Server::accept_request`
(which `Server::accept` wraps, and which moq-tokio and moq-relay's io_uring
path call) and `server::Handshake::ok` are `#[kio::machine]` poll methods with
one-line async wrappers. moq-tokio's `Setup::deadline`, `within`, `Deadline::bound` and its
window widening call this instead of implementing it.

## Plan

Part of [Session policy lives in moq-net](/quest/m1/session-policy/README.md);
see it for the decisions and research.

- Folded in 2026-11-04 (maintainer): the poll-first server-machines quest
  ([last version](https://github.com/moq-dev/moq/blob/e6be13d5f8/quest/m1/poll-first/server-machines.md)).
  Convert `Server::accept` and `Handshake::ok` to poll machines, following
  `kio::machine`'s conventions and [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284);
  if the handshake's poll form makes either a loop, hand-write that `poll_*`
  and say so. The policy then moves onto those machines rather than the async
  bodies they replace.
- moq-net already bounds the reply: `Handshake::with_deadline` refuses a SETUP
  reply the peer never takes. Extend that deadline back over SETUP and
  admission rather than adding a second one. The transport handshake runs
  before moq-net has a session, so the caller bounds it against the same
  instant, as moq-tokio's `within` does today.
- WebRTC's receive-window exception (`qmux_limits` in moq-tokio's `server.rs`)
  becomes a property the transport reports, not a special case in the accept
  path.
- Keep today's defaults (10 s accept) and error codes; tests cover a deadline
  hit at each phase and widening on admit for QUIC, qmux and a transport that
  can't widen.

Public API: breaking in moq-net (`Server`, `Handshake`, the transport session
trait), with MIGRATE notes. Wire: none.

## Required

- [Generated async wrappers](/quest/m1/poll-first/async-feature.md) - gives the handshake a poll form these machines wait on

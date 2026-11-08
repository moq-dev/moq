# [S] A failed client socket ends a WebRTC session with an error

## Goal

When the UDP socket under a moq-webrtc client session fails to receive,
`Client::publish` and `Client::subscribe` end with `Error::Io` carrying the
socket's `io::Error`, instead of `Ok` as if the peer had disconnected. A
`p2p` connection that fails this way hands its opener `p2p::Error::Io` before
its channel opens, and fails the MoQ session over an opened channel with
`qmux::Error::Io`, instead of `Closed`. A peer disconnect still ends `Ok`.

## Plan

Planned 2026-11-08 from [moq-dev/moq#8268](https://github.com/moq-dev/moq/pull/8268)'s
follow-ups, where CodeRabbit raised it and the author deferred it as
pre-existing.

Where it goes wrong today:

1. `ice::reader` (`rs/moq-webrtc/src/ice/wire.rs`) logs a `recv_from` error
   and breaks, dropping its queue's sender. Client sessions (`session::bind_udp`)
   and the `p2p` transport (`p2p::bind`) use it.
2. `Agent::next` (`rs/moq-webrtc/src/ice/agent.rs`) sees the queue end and
   fails with `ice::Error::Closed`, the same variant as an ICE disconnect or
   a dead `Rtc`.
3. `Session::run_loop` (`rs/moq-webrtc/src/session.rs`) maps `Closed` to
   `Ok(())` (with #8268; before it, to `Error::SessionClosed`, which
   `session::end` folded into `Ok`). `p2p`'s `Error::ice` maps it to its own
   `Closed`.

The cause no caller can see is step 1: the reader drops the error. Fix it there.
The reader hands its `io::Error` to the agent, for example as the queue's last
item, and the agent fails with a new `ice::Error` variant named by role (such as
`Socket`). `Agent::next` returns a clone of the stored failure on every later
call, so `ice::Error` stays `Clone` and the variant holds an `Arc<io::Error>`.
The session maps it to the existing `moq_webrtc::Error::Io`, which is already
documented as covering a receive error on the media socket. `p2p`'s
`Error::ice` maps it to `p2p::Error::Io`, whose doc widens from bind and STUN
failures to cover it. `io::Error::new(err.kind(), err)` rebuilds an owned error
from the `Arc` that keeps its kind and source. Once `p2p`'s channel has opened,
the driver drops the channel's `Inbox`, which its `Reader` reads as
`qmux::Error::Closed`. On a socket failure the driver instead pushes
`Err(qmux::Error::Io(arc))` into that `Inbox` first, since `Inbox::push`
already carries errors. Don't match on error kinds to decide which errors are
fatal (AGENTS.md).

Boundary: the server's shared-socket `server::mux::demux` logs and continues on
every recv error, since one transient error, such as an ICMP unreachable,
must not end every session on the socket. It is out of scope. Do check whether
the client reader has the same transient case. On Windows an unconnected UDP
socket reports an ICMP port unreachable as `WSAECONNRESET` unless
`SIO_UDP_CONNRESET` is off. If tokio leaves it on, a client's ICE check to a
dead candidate ends the session today. Turn it off when the socket is bound,
as a known platform capability, rather than ignoring that error at runtime.

Reproducing test, failing on main first. On Linux, a connected UDP socket
reports a loopback ICMP port unreachable as `ECONNREFUSED` on its next
receive, with no rate limit on loopback. Connect a socket to a closed loopback
port, send once, and feed the socket to `ice::reader` and an agent, as
`agent.rs`'s `unpolled` fixture does. Then assert that the agent and the
session end with the socket error, not `Closed` or `Ok`, and that an opened
`p2p` channel's reader returns `qmux::Error::Io`. Keep the existing
peer-disconnect path asserting `Ok`.

Public API: none. The errors map to existing `Io` variants, and the new
`ice::Error` variant is crate-private. Wire: none.

## Related

- [A typed DTLS handshake timeout error](/quest/m3/webrtc/dtls-timeout.md) - also adds an `ice::Error` variant mapped through the session loop and `p2p`

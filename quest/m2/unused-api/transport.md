# [M] The transport crates publish only what something calls

## Goal

moq-tokio, qmux, moq-congestion, and moq-sock publish no item that nothing
outside its own crate and tests calls. What is left of MoQ over a WebRTC
data channel is gone. moq-tokio's in-tree QUIC driver and WebTransport layer
are reachable only through a test-support feature, so the crate's stable API
is MoQ. Each item below is removed, made crate-private, or gated, unless
the first step finds a caller.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Remove or hide every item below before the cut, one sweep per area
  (rejected: shipping them and breaking again; `#[doc(hidden)]`, which the
  repository bans). A removal after 0.22 is a breaking release; an addition
  later is free.
- ✅ Each item is re-checked for a caller in Kixel and moq.pro first, and
  kept if one uses it. The audit could read their quests, not all of their
  code.

Do first: for each item, grep both downstream checkouts for the name. Record
the command and its result in the PR body, one line per item. An item with a
caller stays public, with the caller named beside it.

The items, with the grep that showed no caller in this repository:

1. **`moq_tokio::quic::driver` and `quic::web`**
   (`rs/moq-tokio/src/quic.rs:21`, `:24`): quinn's async layer and a
   WebTransport library, 161 of the crate's roughly 535 public items by
   the audit's count. `grep -rn 'quic::driver::\|quic::web::' rs` outside
   `rs/moq-tokio/src` and `rs/moq-uring/src` finds tests, benches, the
   interop example, and one field moq-relay already gates on its own
   `test-support` feature (`rs/moq-relay/src/cluster.rs:1055`).
   [moq-dev/moq#5664](https://github.com/moq-dev/moq/pull/5664) made them
   `#[doc(hidden)] pub` saying "they are not a stable API";
   [moq-dev/moq#8392](https://github.com/moq-dev/moq/pull/8392) banned
   `doc(hidden)`, which turned them into documented API, and
   `doc/concept/transport.md:37` now advertises `quic::web` as "a
   WebTransport library usable without MoQ". The
   [runtime-free driver](/quest/m4/quic-sansio-driver.md) rebuilds both after
   the cut, so this is the surface most likely to break next. Gate both
   behind one documented feature for tests (moq-tokio has `sim`, moq-relay
   has `test-support`; use one name for both crates) and drop the doc line.
   `moq_uring::quic::web` stays public: moq-relay's io_uring path calls it
   (`rs/moq-relay/src/uring.rs:704`).
2. **`Client::connect_with`** (`rs/moq-tokio/src/client.rs:374`),
   **`Listener::accept_with`** and **`Listener::admission`**
   (`rs/moq-tokio/src/server.rs:770`, `:785`): take a qmux session
   established elsewhere. `grep -rn 'connect_with\|accept_with' rs` finds no
   caller outside the two files since
   [moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771) removed
   peer-to-peer. Delete all three, and the doc lines that name data channels
   (`rs/moq-tokio/src/lib.rs:9`, `server.rs:761`, `:1515`).
3. **`moq_tokio::Transport::WebRtc`** (`rs/moq-tokio/src/transport.rs:17`):
   "a WebRTC data channel using qmux framing". Nothing constructs it; only
   `accept_with`'s caller could. Delete it with its arm in the relay's
   mapping (`rs/moq-relay/src/auth.rs:1295`). moq-ffi's `MoqTransport`
   never mirrored it (`rs/moq-ffi/src/server.rs:443-455` refuses it as
   unsupported). `moq_auth::Transport::WebRtc` stays: WHIP and WHEP
   sessions report it.
4. **`qmux::Config::fixed_receive_window`** (`rs/qmux/src/config.rs:86`):
   set only by `rs/qmux/src/session/tests.rs:285`. Delete the field and the
   two checks that read it (`rs/qmux/src/session.rs:243`, `:414`).
5. **`moq_tokio::tls::Peers`** and `listen.tls.peers`
   (`rs/moq-tokio/src/tls.rs:414`, `:1332`): a live set of pinned client
   fingerprints, from the mesh era. It is settable only from code, and
   `grep -rn 'tls::Peers' rs` outside moq-tokio finds one test
   (`rs/moq-relay/src/web.rs:2219`). This is the item most likely to have a
   Kixel caller, since its mesh pins fingerprints: keep it if so. The
   client-side pin, `connect.tls.fingerprint`, is separate and stays.
6. **`Controller::paces_probe` and `Controller::on_probe_target`**
   (`rs/moq-congestion/src/lib.rs:147`, `:160`): overridden only by GCC
   (`rs/moq-congestion/src/gcc/mod.rs:502`, `:506`), which compiles under
   `lab`. moq-quic calls them in production, where they are always `false`
   and a no-op (`rs/moq-quic/src/connection/bandwidth_probe.rs:245`,
   `connection/mod.rs:1661-1680`, `:3457`). Gate the two methods and their
   call sites on `lab`. A default trait method is additive later; a removal
   is a break of a crate that publishes for the first time at this cut.
7. **moq-sock's ICMP error queue**: `enable_transport_errors`,
   `recv_transport_error`, `TransportError`, and `TransportErrorPayload`
   (`rs/moq-sock/src/udp/unix.rs:225`, `:253`, `udp/mod.rs:172`, `:197`,
   with the reader in `udp/linux.rs`). `grep -rn
   'transport_error\|TransportErrorPayload' rs` outside `rs/moq-sock` finds
   nothing. Delete them with the fuzz seeds that feed the reader, then
   delete [the truncated error-queue quest](/quest/m5/errqueue-truncation.md)
   with `quest rm`, since it repairs code this removes.
8. **moq-sock's buffer size accessors**: `set_send_buffer_size`,
   `set_recv_buffer_size`, `send_buffer_size`, and `recv_buffer_size`
   (`rs/moq-sock/src/udp/unix.rs:289-318`, and the Windows twins). Called
   only by `rs/moq-sock/src/bind.rs:206-207` and the crate's tests. Make
   them crate-private.

Already settled since the audit, so not an item: the unreachable
`TcpTls::new` under a `cfg` that could never hold went with the feature
flags in [moq-dev/moq#8852](https://github.com/moq-dev/moq/pull/8852), and
moq-webrtc's `p2p` and moq-relay's `stun` features went in
[moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771). The
[API review](/quest/m3/release/api-review.md) still lists all three.

Left alone, with the reason: `moq_tokio::mdns` stays where it is (see
[transport names](/quest/m2/transport-names.md)); plaintext `tcp://` and
`unix://` are small and a same-host sidecar is a plausible use; kio's
`PushError` and `Queue::poll_push_with` have no outside caller but belong to
a foundation crate whose surface is otherwise tight.

For the PR: one commit per item keeps the review and any revert small.
Update each crate's migration note and `just api-diff`'s expectations. Run
`just check`, and `just test interop --all` since item 1 moves what the
interop example and moq-uring's tests import.

Public API: breaking removals in moq-tokio (items 1 to 3 and 5), qmux (4),
and moq-sock (7, 8); moq-congestion (6) publishes without two trait
methods. The bindings are unchanged. Wire: none.

## Related

- [Transport names](/quest/m2/transport-names.md) - adds `Tls` to the enum item 3 trims
- [A runtime-free QUIC driver](/quest/m4/quic-sansio-driver.md) - rebuilds the modules item 1 hides

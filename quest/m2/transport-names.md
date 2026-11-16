# [S] A TLS stream link reports tls, and @moq/qmux has one stream limit

## Goal

A session over `tls://` reports its transport as `tls`, in
`moq_tokio::Transport`, in `moq_auth::Transport`, in the auth request an
endpoint receives, and in the bindings, so an auth server, a log line, and
the relay's own checks can tell an encrypted stream link from plaintext
`tcp://`. `@moq/qmux` takes one `maxStreams`, applied to each direction, as
Rust's `Limits::max_streams` and `@moq/net`'s `websocket.maxStreams` already
do.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Add `Tls` to both `Transport` enums and report it for `tls://`
  (rejected: only fixing the moq-tokio doc that says "plaintext"). Both
  enums are non-exhaustive, so the variant is additive in Rust, but
  changing what an existing link reports moves every policy and metric
  keyed on `"tcp"`. That is cheapest before the name is announced.
- ✅ `@moq/qmux` takes a single `maxStreams` (rejected: also nesting its
  windows under `limits` to mirror Rust; keeping the bidirectional and
  unidirectional pair).
- LAN discovery stays in moq-tokio: the audit proposed moving
  `moq_tokio::mdns` into moq-relay, and the maintainer did not take it.
  Don't reopen it without a new reason, such as a measured build cost.

Facts found by the audit:

- `moq_tokio::Transport::Tcp` is documented "A plaintext TCP connection
  using qmux framing" (`rs/moq-tokio/src/transport.rs:11-12`), yet a
  `tls://` dial returns it (`rs/moq-tokio/src/client.rs:496-506`) and the
  accept side reports it for every stream link
  (`rs/moq-tokio/src/server.rs:1298`). `moq_auth::Transport::Tcp` says
  "plaintext or TLS" (`rs/moq-auth/src/request.rs:186-187`).
- The relay maps one enum onto the other (`rs/moq-relay/src/auth.rs:1288`),
  so an auth endpoint sees `"tcp"` for an mTLS cluster link unless a client
  certificate happens to be attached. `moq`'s `is_public_transport`
  (`rs/moq-cli/src/main.rs:226-231`) treats `Tcp` as always public.
- The strings are a contract: `Transport::as_str`
  (`rs/moq-tokio/src/transport.rs:22-33`), the serde names in moq-auth
  (`rs/moq-auth/src/request.rs:212`), `js/auth/src/contract.ts:35`, and
  `MoqTransport` in moq-ffi (`rs/moq-ffi/src/server.rs:449`).
- `@moq/qmux`'s `Config` has `maxStreamsBidi` and `maxStreamsUni`
  (`js/qmux/src/session.ts:33-35`, `js/qmux/api/index.api.md:19-20`). Its
  one consumer sets both from one number
  (`js/net/src/connection/connect.ts:549-560`), and Rust has one
  `Limits::max_streams` (`rs/moq-quic/src/limits.rs:21`).
  The package moves into this repository at this cut, so the rename rides
  a break it already has.
- `ws://` and `wss://` share `WebSocket` in the same way. Not picked: a
  WebSocket's TLS belongs to the HTTP server in front of it, which the relay
  often cannot see.

For the PR:

- Add `Tls`, spelled `"tls"`, to `moq_tokio::Transport` and
  `moq_auth::Transport`. Report it from the `tls://` dial and from the TLS
  branch of the stream acceptor. Fix the `Tcp` docs to say plaintext in both
  crates. Extend the relay's mapping and `is_public_transport` (a TLS link
  is public the same way plaintext TCP is; say so in its doc).
- Mirror it in `@moq/auth`'s contract enum and in `MoqTransport` with every
  wrapper. If [open-set selectors](/quest/m2/ffi-open-selectors.md) has
  landed, `MoqTransport` is already an object and this adds a constructor.
- Grep for consumers of the `"tcp"` string: relay metrics labels
  (`rs/moq-relay/src/internal.rs` uses `"tcp"` for the listener, which
  stays, since the listener is one TCP port), stats labels, `doc/bin/relay`,
  and `doc/concept/transport.md`. The auth docs get a line: a server keying
  policy on `transport == "tcp"` for cluster links over TLS now sees `"tls"`.
- `@moq/qmux`: one `maxStreams?: bigint` on `Config` and its default; delete
  the pair; `@moq/net` passes it once. Update `js/qmux/api/index.api.md`,
  the README, and the migration note.
- Tests: a `tls://` session reports `tls` on both ends and in the auth
  request; a `tcp://` session still reports `tcp`; `@moq/qmux` advertises
  the one limit for both directions.

Public API: additive in Rust (`Transport::Tls` on two non-exhaustive
enums) and a new binding selector; breaking for `@moq/qmux`
(`maxStreamsBidi` and `maxStreamsUni` become `maxStreams`). Behaviour: the
transport string for `tls://` changes from `tcp` to `tls` in auth requests
and logs. Wire: none.

## Related

- [Open-set selectors in the bindings](/quest/m2/ffi-open-selectors.md) - makes `MoqTransport` able to grow without a break

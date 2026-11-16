# [M] moq-net, @moq/net, and the bindings have no socket endpoints

## Goal

No package this repository publishes has a socket endpoint: `moq_net::socket`
is deleted from Rust, `Socket` from `@moq/net`, the socket handles from
moq-ffi and moq-c, and the wrapper from Python, Swift, Kotlin, Dart, and Go,
with `doc/concept/socket.md`, the interop sockets lane, and
`socket::is_endpoint` in every language. What exists in moq-net only for
sockets goes with them, except the one private call Kixel's copy needs, which
becomes public.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ socket
endpoints leave for Kixel
([the epic](/quest/m2/sockets/README.md) has the reasons and the rejected
options). `is_endpoint`, which the unused-API sweep had listed for the FFI,
leaves here instead.

Delete, as listed at `80ca9787e3`:

- Rust: `rs/moq-net/src/model/socket.rs` and its `pub mod socket`
  (`rs/moq-net/src/model/mod.rs:7`), `rs/moq-net/tests/integration/socket.rs`,
  `rs/moq-relay/tests/integration/socket_auth.rs`, and the CHANGELOG and
  `doc/lib/rs/moq-net.md` entries.
- JS: `js/net/src/socket.ts`, `socket.test.ts`, `js/net/bench/socket.ts`,
  the `Socket` export (`js/net/src/index.ts:39`), `js/net/api/index.api.md`,
  and `doc/lib/js/net.md`.
- Bindings: `rs/moq-ffi/src/socket.rs` (and the `MoqSocketEndpoint` entry in
  `rs/moq-ffi/uniffi.toml`), `rs/moq-c/src/socket.rs` and its `moq.h`
  functions, `py/moq-rs/moq/socket.py` with `tests/test_socket.py` and the
  `sockets` bench in `benches/ffi.py`, `swift/Sources/Moq/Socket.swift`,
  `kt/moq/.../socket/Socket.kt`, `dart/moq/lib/socket.dart`,
  `go/wrapper/socket/`, their tests (`rs/moq-ffi/src/test.rs`,
  `rs/moq-c/src/test/parity.rs`), README and docs mentions
  (`rs/moq-c/README.md`, `py/moq-rs/README.md`, `doc/lib/py/index.md`), and
  the generated Dart file's socket classes.
- Interop: `test/interop/socket.sh`, the `--sockets` lane and `run_sockets`
  in `test/interop/interop.sh`, `clients/rust/socket.rs`,
  `clients/rust/socket-js.rs`, `clients/js-native/socket.ts`,
  `clients/python/sockets.py`, the `interop-socket` and `interop-socket-js`
  examples in `rs/moq-cli/Cargo.toml`, and the README's sockets paragraph.
- Docs: `doc/concept/socket.md`, its sidebar entry
  (`doc/.vitepress/config.ts:120`), and its line in `doc/concept/index.md`.
- Each package's `MIGRATE.md` entries that describe a socket API change
  (`rs/moq-ffi`, `rs/moq-tokio`, `js/net`, `py/moq-rs`, `kt/moq`, `cpp/moq`):
  nothing published ever had sockets, so the entries go rather than gain a
  removal note. Check `origin/release` for each before deleting it.

What exists only for sockets, to check before deleting:

- `broadcast::Producer::reserve_unlisted_track`
  (`rs/moq-net/src/model/broadcast.rs:406`), `pub(super)`: a track a peer
  who knows its name finds, never announced or listed. Sockets are its one
  caller. Kixel's copy is built on moq-net's public API and needs exactly
  this. Recommended: make it public, with Kixel as its named consumer,
  instead of deleting it; the alternative is that Kixel's session tracks
  appear in track listings.
- `origin::Consumer::hidden_withheld` and the hold a pre-lite-07 client
  session takes on it (`rs/moq-net/src/lite/session.rs:311-315`), which make
  a dial fail `Unsupported` while a hop cannot carry hidden broadcasts.
  Sockets are its one reader (`socket.rs:442`). Recommended: delete it,
  unless Kixel's port names a need.
- Hidden (`.`-named) broadcasts themselves are not socket-only: HLS, the CLI,
  and `RouteOptions::hidden()` use them
  (`rs/moq-hls/src/server/mod.rs:191`, `doc/concept/identity.md:196`). They
  stay.
- `rs/moq-net/src/auth.rs` tests use `.socket` only as a pattern string;
  they stay.

For the PR:

- Start by grepping for `socket::`, `Socket.Endpoint`, `MoqSocket`,
  `moq_socket_`, `dial_track`, `accept_track`, `.server/`, and `.client/`
  across `rs`, `js`, `py`, `swift`, `kt`, `dart`, `go`, `cpp`, `demo`,
  `test`, and `doc`; the list above is that grep on the base, and both demos
  must already be off it.
- Record the PR's parent commit in the PR body: it is the reference Kixel
  ports from.
- If [the closed-endpoint flake](/quest/m0/test-flakes-3/py-socket-abort.md)
  is still open, delete it here: its test leaves.
- Run `just test interop --all` (moq-ffi changes) and `just api-diff`.

Public API: breaking for `main`-only API in every language; nothing on
`release` has sockets. Adds `reserve_unlisted_track` if kept. Wire: none
(sockets were a path and track naming convention).

## Required

- [MoQ Boy viewers publish their commands again](/quest/m2/sockets/boy.md) - the emulator and its element stop using `moq_net::socket` and `@moq/net`'s `Socket`
- [CARLA drivers publish their controls again](/quest/m2/sockets/carla.md) - the worker stops using the Python wrapper and the player `@moq/net`'s `Socket`

## Related

- [moq-net publishes no item without a caller](/quest/m2/unused-api/net.md) - the sweep this keeps `is_endpoint` out of

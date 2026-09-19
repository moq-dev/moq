# [L] One QUIC backend

## Goal

noq is the only QUIC stack in the tree. `moq-tokio`, `moq-uring`, `moq-relay`,
`moq-cli`, `moq-bench`, and `moq-boy` compile no quinn or quiche code, carry
no `quinn`, `quiche`, `io-uring-quinn`, or `io-uring-quiche` feature, and
offer no backend selection at runtime. The iroh backend stays, because it runs
on noq. The qmux fallbacks over TCP, Unix sockets, and WebSocket stay
untouched.

Supporting several stacks bought nothing: every transport feature MoQ needs
(ACK progress, reliable reset, hierarchical scheduling, probing, deadlines)
lands in [our fork of noq](/quest/m2/quic/fork.md) and would be missing on
the others. quiche is unmaintained upstream. moq.pro still builds `main` with
quinn; it moves to noq with the dev merge, and nothing in its config names a
backend that survives.

## Plan

Branch from `dev`. The break is a removed public feature and two removed
config fields, so it cannot land on `main`.

Delete, in `rs/moq-tokio`: `src/quinn.rs`, `src/quiche.rs`, the `quinn` and
`quiche` features and their optional dependencies (`quinn`,
`web-transport-quinn`, `web-transport-quiche`, `rustls-native-certs`), the
`QuicBackend` enum, `--listen-backend` / `--connect-backend`, the
`listen.backend` / `connect.backend` TOML fields, `default_quic_backend()`,
and the `Quinn` / `Quiche` variants of the pending-request enum in
`src/server.rs`. Keep the `noq` feature name so a consumer can still build
without QUIC (tcp-only serves qmux), and keep the `aws-lc-rs` / `ring`
compile_error.

Delete, in `rs/moq-uring`: `src/quic/quiche/`, the `quinn` and `quiche`
features, `quinn-proto`, `quiche`, and `boring` from the manifest, and the
`use noq_proto as quinn_proto` alias: rename `src/quic/quinn/` to
`src/quic/noq/` and name the types for what they are. Replace the raw quiche
peer in `tests/support/quiche.rs` and `benches/echo_quiche.rs` with a tokio
noq peer; cross-implementation interop is `just test smoke-full`'s job.

Forward the deletion through every consumer manifest: `moq-relay`
(`quinn`, `quiche`, `io-uring-quinn`, `io-uring-quiche`), `moq-cli`,
`moq-bench`, `moq-boy`, and the `moq-native` shim's empty feature list. Drop
the per-backend `send_window` refusal and the "each backend ships a different
BBR generation" paragraph; there is one BBR now (v3).

Docs: `doc/bin/relay/config.md` (backend selection, `[runtime] workers`,
`io_uring`), `doc/bin/relay/http.md` (certificate selection), `doc/bin/cli.md`
(the `cargo install` feature list), `doc/lib/rs/index.md`, `doc/lib/c/index.md`
(the `backend` field of `moq_client_config`, which goes through moq-ffi and
every binding: report the wire and API impact), `rs/moq-tokio/README.md`, and
the `## Backends` table in `rs/moq-uring/README.md`. Search the repository for
`--listen-backend`, `io-uring-quinn`, and `features "` invocations and fix
every example.

Tests: the existing backend integration tests run once, on noq; delete the
per-backend matrix. `just check`, `just test`, the nightly feature-subset
build, and `just test smoke-full` pass. Record the deleted line count in the
PR.

## Closes

- [#3124](https://github.com/moq-dev/moq/issues/3124) - the matched
  noq-versus-quiche measurement is moot: the decision was made without it
- [#2296](https://github.com/moq-dev/moq/issues/2296) - quiche parity gaps
  vanish with the backend
- [#2853](https://github.com/moq-dev/moq/issues/2853) - the pinned-port dial
  bug is quiche-only
- [#2847](https://github.com/moq-dev/moq/issues/2847) - the cwnd/rtt estimate
  is quinn-only; noq reports BBR3's estimate

## Related

- [The fork](/quest/m2/quic/fork.md) - why one stack: every transport feature
  lands there
- [io_uring feature check](/quest/m2/check-uring-feature.md) - checks the
  `io-uring` feature once this rename lands

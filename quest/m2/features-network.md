# [L] Always compile the network stack

## Goal

The transports, LAN discovery, STUN, and the io_uring runtime compile into
every build of the crates that offer them, picked at runtime by URL scheme,
flag, or config instead of by Cargo feature. These features go away:

- moq-tokio: `net`, `driver`, `quic`, `websocket`, `tcp`, `uds`, `qmux`,
  `watch`, `mdns`, and the private `_transport` and `_certs`. It keeps
  `jemalloc` (a C build) and `sim` (test-only), and `qlog` until
  [Always compile qlog](/quest/m2/features-qlog.md) decides it.
- moq-relay: `quic`, `_quic`, `websocket`, `uds`, `cluster-lan`, `stun`,
  `io-uring`, and `_uring`.
- moq-cli: `quic`, `websocket`, and `cluster-lan`.
- moq-bench and moq-boy: `quic`, `websocket`, and `tcp`.
- qmux: `tcp`, `tls`, `uds`, `ws`, and `wss`.

Unix sockets compile on `cfg(unix)` and io_uring on `cfg(target_os =
"linux")`, as platform gates rather than features.

## Plan

Found by the [Cargo feature review](/quest/m3/release/feature-flags.md) (first
pass, 2026-11-14). The `# Features` rules in `rs/AGENTS.md` keep a feature only
for a vendor integration or a system dependency. None of these is either: every
crate they gate is pure Rust. Measured with `cargo tree -e normal`, moq-relay's
default `quic`, `websocket`, `uds`, and `cluster-lan` add 10 of its 278
crates (mdns-sd, if-addrs, flume, socket-pktinfo, web-transport-proto, sfv, and
four small helpers); moq-tokio's `websocket`, `tcp`, and `uds` add 12 over
`quic` alone. Against that, `quic`, `websocket`, `tcp`, and `uds` hold about
660 `cfg` sites in moq-tokio and 160 in moq-relay, plus the "every subset
compiles" matrix in `just rs features` (`rs/justfile`) and private markers
(`_quic`, which only `quic` enables, and `_transport`) that exist only to make
those subsets compile. No shipped consumer turns them off: the release binaries,
Nix, moq-ffi, moq-gst, and moq-c all build every transport.

moq-tokio's `driver` without `net` (the QUIC driver without moq-net) has no
consumer: every enabler also turns on a transport, which turns on `net`. moq-net
becomes an unconditional dependency.

Keep each runtime refusal that names a missing feature only where a platform can
still lack it (io_uring off Linux, Unix sockets off Unix), and reword it to name
the platform. `config.rs`'s "needs moq-relay built with the `quic` feature"
checks go away, with the tests that assert them, such as
`workers_require_a_quic_backend` in `rs/moq-relay/src/relay.rs`.

Breaking for anyone who names a removed feature in a manifest: Cargo errors on
an unknown feature. The PR's Impact section says to delete those names from
`features = [...]` and `--features`; nothing replaces them.

Decided 2026-11-14 (maintainer, /quest-complete): ✅ keep this quest in m2
(rejected: drop it), and ✅ compile moq-relay's io_uring runtime into every
Linux build (rejected: keep `io-uring` as an opt-in feature). It is pure Rust
(the `io-uring` crate, no liburing), already selected at runtime by
`--runtime-io-uring`, and moq-relay already carries most of what `_uring`
adds.

Update `doc/bin/relay` and `doc/bin/cli.md` wherever they tell a source
builder to pass one of these features, `bench/run.sh`'s and
`.oss-scanner/Dockerfile`'s `--features moq-relay/io-uring`, and the
`rs/justfile` recipes that name a removed feature: `features` (moq-cli's
`websocket`), `uring`, and `uring-check` (moq-relay's `io-uring`).

## Related

- [Cost audit](/quest/m3/audit-cost.md) - owns per-crate build cost, which this
  trades a little of for a smaller test matrix

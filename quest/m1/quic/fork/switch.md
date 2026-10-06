# [L] Switch MoQ onto moq-quic

## Goal

quinn's async layer and `web-transport-moq` live in moq-tokio as modules,
moq-tokio and moq-uring run on `moq-quic`, and no workspace crate depends on
`moq-noq*`. Upstream `noq-proto` remains only for the `iroh` feature. Relay
memory on the bulk and fanout workloads matches `moq-noq`.

## Plan

Decided 2026-10-06: `moq-quic` (quinn-proto) is the only crate the fork adds.
quinn's async layer is imported into moq-tokio as a module rather than a
`moq-quic-tokio` crate, tokio-only: drop quinn's `Runtime` abstraction and
its smol and async-io implementations instead of carrying them. Its UDP
sockets come from `moq_sock::udp`.
Keep the first commit verbatim from upstream (quinn's `quinn/src`) so a
reviewer can diff it, and extend the cherry-pick recipe in
`rs/moq-quic/README.md` to map `quinn/src/` onto the module.

Move `web-transport-moq` from moq-dev/noq into moq-tokio as a module too. It
derives from web-transport-quinn; its noq-only parts are `PathId::ZERO` and
`path_stats`. Report bandwidth from quinn's `PathStats::bandwidth_estimate`
instead of the current cwnd/rtt guess. moq-uring uses it only in tests,
through moq-tokio.

moq-uring built against both quinn-proto and noq-proto until #3811, behind
about 20 `cfg` lines (stats fields, CID generator, qlog, BBR). Rename
`rs/moq-uring/src/quic/noq` by role and use that history as the map. Keep
moq-uring's `qlog` feature working on quinn's qlog.

Rename the `noq` cargo features by role (`quic` is the recommendation);
confirm the name with the maintainer in the PR. Update every doc and example
that names noq, and run `just test interop --all`.

In the PR, list each carried change from moq-dev/noq's `CHANGELOG-MOQ.md` as
ported (with its quest) or not applicable (with the reason).

Lazy stream slots ([quinn#2601](https://github.com/quinn-rs/quinn/pull/2601),
the change noq took as noq#667) arrived with the import at quinn `7616e6b2`,
so only its measurement remains, and that needs the relay on `moq-quic`.
Re-run #3342's bulk and fanout relay memory workloads after the switch and
report them in the PR. On `moq-noq` they measured 75 MiB (bulk) and 31 MiB
(fanout), against 141 and 97 MiB without lazy slots; `moq-quic` should land
near the former.

## Required

- [Port BBR3](/quest/m1/quic/fork/bbr3.md)

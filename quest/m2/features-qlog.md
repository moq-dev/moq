# [S] Always compile qlog

## Goal

qlog trace capture compiles into every build that has QUIC, and the existing
runtime switches (`--quic-qlog <DIR>`, `--client-quic-qlog <DIR>`,
`quic::Transport::qlog`, moq-ffi's and the C++ wrapper's qlog option) are the
only way to turn it on. The `qlog` features of moq-quic, moq-tokio, moq-uring,
moq-relay, moq-cli, and moq-ffi, and the C++ `MOQ_FFI_QLOG` CMake option, go
away.

## Plan

Found by the [Cargo feature review](/quest/m3/release/feature-flags.md) (first
pass, 2026-11-14). Decided 2026-11-14 (maintainer, /quest-complete): ✅ keep
this quest in m2 (rejected: drop it).

The features exist so "production builds do not pull in the
qlog crate and its serde_json encoder". Measured with `cargo tree -e normal`,
`qlog` adds one crate to moq-relay (`qlog` itself; serde, serde_json, and
serde_with are already there) and 16 to moq-quic alone. It costs about 30 `cfg`
sites in moq-quic and 10 in moq-uring, and it means an operator who hits a
congestion bug on a shipped relay has to rebuild before they can trace it.

Confirm that a connection with qlog off pays nothing on the hot path (the
writer is `None` and no event is built) before removing the gate; if it does
pay, fix that in the same PR rather than keep the feature.

Breaking for anyone who names `qlog` in a manifest or passes
`-DMOQ_FFI_QLOG=ON`: the PR's Impact section says to delete it. A prebuilt
moq-ffi then carries qlog, so `cpp/moq/cmake/moq-ffi.cmake` drops its check
that a prebuilt input lacks it, and moq-ffi's `moq_qlog_supported` (always
true then) is deleted from every binding with its "fails unless supported"
doc on `MoqQuicConfig::qlog`. `doc/bin/relay/config.md` drops "Needs the
`qlog` build feature", `doc/bin/obs.md` drops its `-DMOQ_FFI_QLOG=ON` source
build for traces, and the `rs/justfile` uring recipes drop `moq-relay/qlog`
from their clippy run and `--features qlog` from moq-uring's nextest run.

## Related

- [Always compile the network stack](/quest/m2/features-network.md) - removes
  the other moq-tokio and moq-relay features; expect conflicts in the same
  manifests

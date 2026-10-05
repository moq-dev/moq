# [M] Switch MoQ onto moq-quic

## Goal

`web-transport-moq` lives in `rs/` on `moq-quic-tokio`, moq-tokio and
moq-uring run on `moq-quic`, and no workspace crate depends on `moq-noq*`.
Upstream `noq-proto` remains only for the `iroh` feature.

## Plan

Move `web-transport-moq` from moq-dev/noq. It derives from
web-transport-quinn; its noq-only parts are `PathId::ZERO` and `path_stats`.
Report bandwidth from quinn's `PathStats::bandwidth_estimate` instead of the
current cwnd/rtt guess.

moq-uring built against both quinn-proto and noq-proto until #3811, behind
about 20 `cfg` lines (stats fields, CID generator, qlog, BBR). Rename
`rs/moq-uring/src/quic/noq` by role and use that history as the map. Keep
moq-uring's `qlog` feature working on quinn's qlog.

Rename the `noq` cargo features by role (`quic` is the recommendation);
confirm the name with the maintainer in the PR. Update every doc and example
that names noq, and run `just test interop --all`.

In the PR, list each carried change from moq-dev/noq's `CHANGELOG-MOQ.md` as
ported (with its quest) or not applicable (with the reason).

## Required

- [Port BBR3](/quest/m1/quic/fork/bbr3.md)
- [Lazy stream slots](/quest/m1/quic/fork/stream-slots.md)

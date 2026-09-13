# [M] SRT rejection reason

## Goal

An SRT client the gateway refuses learns WHY on the wire: an unauthorized
credential, a forbidden one, and a failed upstream lookup arrive as distinct
SRT extended reject reasons instead of one generic forbidden.

## Plan

SRT's server rejection reasons already carry the HTTP-shaped distinctions
(`2401` unauthorized, `2403` forbidden, `2502` gateway), and a caller knows
which one applies. `moq_srt` cannot say any of them: `Publish::reject` and
`Subscribe::reject` take no argument and send a fixed
`Server(ServerRejectReason::Forbidden)`.

Give `reject` the reason instead of adding an argument-less shim or a
`reject_with` sibling, so the library stays HTTP-unaware and the caller maps
its own verdict explicitly. Update the in-repo callers
(`rs/moq-srt/src/listen.rs`, `rs/moq-cli/src/srt.rs`) and the embedder example
in `rs/moq-srt/README.md` to pass what they mean.

`ServerRejectReason` is a private `srt_tokio` import today, so the public type
is part of the decision. Re-export it from `moq_srt` (`pub use
srt_tokio::access::ServerRejectReason;`) and take it in `reject`, rather than
making every embedder depend on `srt_tokio` directly or duplicating the SRT
reason list in a new enum. The re-export is the smallest surface, but it
ties the public type identity to `srt_tokio`, so a backend swap still needs an
API migration; a dedicated `moq_srt` enum is only worth it if an embedder
needs backend independence or a reason SRT cannot express.

Prove it over the wire for both a rejected publish and a rejected subscribe
with a non-default reason, alongside the existing coverage that a rejected
connection closes cleanly.

## Related

- [SRT import stats](/quest/m2/srt-import-stats.md) - the same gateway's
  per-stream counters

# [M] Public wall times on moq_clock::Wall

## Goal

Every public wall time in the Rust crates is a `moq_clock::Wall` instead of a
`std::time::SystemTime`, and the per-crate clippy ratchets ban the
`SystemTime` type wherever it no longer crosses a third-party API.

## Plan

The remaining public `SystemTime`s, as of
[#6135](https://github.com/moq-dev/moq/pull/6135):

- hang: `catalog::Clock::wall_clock` and `echo::Arrival::wall`, plus the
  snapshot serializer.
- moq-mux: `Clock::at`, `Clock::wall_clock`, `catalog::Timebase::place`, and
  the catalog producer's wall argument; this ripples into moq-gst's sink,
  moq-audio's capture, and moq-hls.
- moq-hls: the manifest and playlist `program_date_time`,
  `availability_start`, `publish`, and `Renditions::anchor`.
- moq-quic: `TimeSource::now` and the token log's issue times.
- moq-relay: the session's `started`.
- moq-tokio: the certificate `expiry`.
- moq-auth: `Claims::{expires, issued, not_before}` with their `with_*`
  setters, and `Grant::expires`.
- moq-cli and moq-bench: private wall times, which can move freely.

`Wall` covers 1970 to 2554, so a catalog wall past that range is an error, as
it already is on Windows. A third-party API on `SystemTime`, such as
str0m's sender reports in moq-rtc, converts at its boundary with
`From<Wall>` and `TryFrom<SystemTime>`.

Expiries saturate instead (decided 2026-10-22, maintainer, from an external
API audit of [#6298](https://github.com/moq-dev/moq/pull/6298)): a
certificate `not_after` or an auth expiry past 2554 becomes the latest
representable `Wall`, and one before 1970 becomes `Wall::UNIX_EPOCH`, so
`tls::PeerIdentity::expiry` and the auth claims stay infallible and rcgen's
default year-4096 certificates keep working. No representable `now` can compare differently against the
saturated value, so this loses nothing; refusing them would turn a
representation limit into a validity rule for valid identities. Presentation
walls (catalogs, timelines, HLS) still refuse, since their value is shown or
used for arithmetic, not only compared.

Public API: breaking in hang, moq-mux, moq-hls, moq-quic, moq-relay, moq-auth, and
moq-tokio, each with a `MIGRATE.md` entry. Wire: none.

## Related

- [Time test sweep](/quest/m2/time-test-sweep.md) - the workspace-wide lint follows this

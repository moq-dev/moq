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
- moq-cli and moq-bench: private wall times, which can move freely.

`Wall` covers 1970 to 2554, so a catalog wall past that range is an error, as
it already is on Windows. A third-party API on `SystemTime`, such as str0m's
sender reports in moq-rtc, converts at its boundary with `From<Wall>` and
`TryFrom<SystemTime>`.

Public API: breaking in hang, moq-mux, moq-hls, moq-quic, moq-relay, and
moq-tokio, each with a `MIGRATE.md` entry. Wire: none.

## Related

- [Time test sweep](/quest/m2/time-test-sweep.md) - the workspace-wide lint follows this

## Progress

Candidate: [moq-dev/moq#6298](https://github.com/moq-dev/moq/pull/6298).
The implementation also covers auth claim/grant/certificate times and the
Rust epoch wall-time view, which were omitted from the initial inventory.
Numeric epoch getters keep UUIDv7's full range; protocol fields and JSON
units are unchanged. Rust auth decoding now enforces Wall's portable range.

The PR remains draft for the shared optional Unix-seconds serde adapter,
fallible epoch and certificate wall views, and boundary refusal decisions.
Keep this quest until those public API and range choices are settled.

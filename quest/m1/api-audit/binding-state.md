# [M] Binding connection state

## Goal

A binding caller watches one connection state instead of combining a
blocking status watcher, a polling loop, a reconnect counter, and terminal
closure. `MoqSession::state()` returns a snapshot (presence, connect count,
last failure, version) and `state_changed()` waits for the next one,
mirroring moq-tokio's `Monitor::snapshot`. `status()`, `connects()`, and both
`failure()` polls are gone.

## Plan

Decided 2026-10-22 (API audit). `rs/moq-ffi/src/session.rs` has
`MoqClient::failure` (#6131) and `MoqSession::failure` (#5723), both new and
polled because successive failures do not change `status()`. OBS combines
all of them (`cpp/obs/src/moq-output.cpp`, `moq-source.cpp`). Kotlin, Dart,
and Go hide the `Client`, so the first-dial failure is unreachable there.

- `MoqClient::connect` returns the session at once in a connecting presence,
  so the pre-session failure is part of the same state and
  `MoqClient::failure` goes away.
- Name fields after moq-tokio's `Monitor` (`presence`, not `status`).
- Update every wrapper, OBS, and `doc/lib/*`.
- The generated C package (`c/moq`) probes `moq_client_failure` and maps the
  hand-written `moq_session_failure` to it in `c/moq/MIGRATE.md`; point both
  at the state snapshot's last failure.

Public API: moq-ffi, moq-c, every wrapper, OBS breaking. Wire: none.

## Related

- [Go first-dial failure](/quest/m2/go-first-dial-failure.md) - becomes unnecessary once the state carries the first failure; delete it when this lands

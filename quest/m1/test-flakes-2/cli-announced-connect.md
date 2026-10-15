# [S] moq-cli's announced fixture connects under load

## Goal

The `announced` tests in `rs/moq-cli/src/announced.rs`, starting with
`fails_when_the_session_ends`, pass under a loaded `just check`, fixed at
their cause. Their fixture's publisher connects to a relay that has already
reported ready.

## Plan

Seen once, locally under `cargo test`, while landing #5575 (2026-10-15).
`fails_when_the_session_ends` failed before reaching its assertion: the
fixture's publisher connect (`Fixture::new`) returned `TransportRace`,
meaning both the QUIC dial and the WebSocket fallback failed
(`race_transport_connect` in `rs/moq-tokio/src/client.rs`). A rerun passed.
Not found in CI. The fixture shares `moq_relay::test_relay` with the other
tests in the file, so all three are exposed.

A relay that reported ready refusing both transports is not expected under
load, so start by reproducing it (the test in a loop beside a CPU hog, or
`--stress-count` while `just check` runs) and reading both causes the
`TransportRace` error carries. Fix the dial or the relay if either gives up
too early; fix the fixture only if it starts dialing before the relay can
accept. No retry, no wider timeout.

Public API: none expected. Wire: none.

## Related

- [Auth serve signals](/quest/m1/test-flakes-2/auth-serve-signals.md) - another moq-cli test that fails only under load

# [S] Bench harness knows its relay owns the port

## Goal

`bench/relay.sh` reports a relay as ready only once that relay holds the port
it was given. A relay that loses the port fails the run at startup with the
relay's log, never as a load generator that silently receives zero groups.

## Plan

`start_relay` picks a random port in 40000-59999, starts the relay, and treats
any TCP connect to that port as readiness. If another process already listens
there, the probe succeeds against it. The relay then exits with "Address
already in use", and the harness only notices when the load generator receives
nothing. This happened once on a busy host during #5277.

A shell harness cannot pick a port and bind it atomically for the relay, so
the open choice is where ownership gets proven. Settle it in the PR:

- Recommended: keep the random port, and after the probe succeeds confirm that
  `$RELAY_PID` is alive and owns both the TCP and UDP listeners on it (for
  example with `ss -lnp`). Otherwise retry with a new port. Bench-only, with no
  relay change.
- Let the relay bind port 0 and report its bound addresses. The relay prints
  neither address today, and QUIC and HTTP would get different ports, so this
  adds relay behavior and docs for a bench-only problem.
- Probe a relay-specific endpoint, such as its certificate fingerprint. This
  proves the HTTP listener but not the QUIC socket.

Cover it in `bench/relay.test.py` (already run by `just gh check`): hold the
chosen port open and expect the harness to retry or refuse.

## Related

- [Benchmark reporting](/bench/README.md) - the harness this fixes
- [Relay profiling](/quest/m2/perf/lock-profile.md) - where the race surfaced

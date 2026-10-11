# [M] Session report parity for OBS

## Goal

A moq-ffi session reports the draft it negotiated and why its latest reconnect
attempt failed, so the OBS dock shows **Protocol** in its stats again and names
the reason (such as unauthorized) while a stream is reconnecting, instead of a
bare "Reconnecting".

## Plan

- `MoqSession` gains the negotiated version, the way moq-c's
  `moq_session_snapshot` carries `protocol`; it changes across reconnects, so
  read it live rather than once at connect.
- The reconnect loop's last failure is visible while it retries. Today
  `status()` only reports `Disconnected`, and the error surfaces only once the
  loop gives up. Keep `status()` a coalesced current state; a separate accessor
  or an error on the `Disconnected` transition are the options to weigh.
- `cpp/obs/src/moq-output.cpp`: fill `ConnectionStats::protocol` and record the
  reconnect failure in `last_failure_*`, which the dock already classifies.
- Cross-package sync for `rs/moq-ffi` wrappers and docs.

Public API: additive on moq-ffi and every binding. Wire: none.

## Related

- [Client settings parity](/quest/m1/obs-client-config.md) - the settings the migration dropped

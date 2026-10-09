# [L] Client settings parity for OBS

## Goal

moq-ffi's `MoqClient` offers every connection knob moq-c's `moq_client_config`
does, and reports the defaults and supported values a settings UI is built
from, so the OBS **Advanced** settings get back the ones the migration to the
generated C++ dropped: protocol version, connect timeout, Happy Eyeballs delay,
SNI override, QUIC idle timeout, keep-alive, GSO, path MTU discovery,
congestion control, and qlog directory. Scene collections that set them apply
again, under the same keys.

## Plan

- moq-ffi: the knobs map onto the same `moq_tokio` `connect::Config` and
  `quic::Config` fields `rs/moq-c/src/client.rs` fills. Settle the shape with
  the maintainer first: one setter per knob, matching the existing `set_*`, or
  a `MoqClientConfig` record whose generated defaults are the library's. The
  record is the recommendation, since it also gives a settings UI its defaults
  in every language, the way `moq_client_defaults` does for C. A
  backend-dependent default (GSO, MTU discovery, congestion control) stays
  absent rather than guessed.
- Also report what moq-c enumerates today: the supported protocol versions
  (`moq_versions`) for the version menu, and whether this build can write qlog
  (`moq_qlog_supported`), so the plugin hides a field it can't honor.
- `cpp/obs/src/moq-settings.cpp`: restore the fields and their `Configure`
  calls, reading defaults from moq-ffi instead of the constants it repeats now
  (`QUIC_MAX_STREAMS_DEFAULT`, the WebSocket defaults). The dock's
  **Advanced** tooltip names the protocol pin again.
- Cross-package sync for `rs/moq-ffi`: the hand-written wrappers' client
  options and `doc/lib/*`; `doc/bin/obs.md` lists the restored settings.

Public API: additive on moq-ffi and every binding. Wire: none.

## Related

- [Session report parity](/quest/m1/obs-session-report.md) - the dock's other gaps after the migration

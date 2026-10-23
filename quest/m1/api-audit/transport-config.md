# [M] Transport config

## Goal

moq-tokio and moq-quic configuration has one meaning for an unset value,
types modes as enums, has no fields that only one other field reads, and
ships no leftover features.

## Plan

### Checkpoint

The original API-audit recommendations were approved 2026-10-22. The current
implementation has one human-readable duration field per setting and `None`
defaults; native keylog paths; internally supervised accepted sessions with
panic diagnostics published by the driver before unwind; the direct QUIC
connection; one `[listen.lb]` path; private test hooks; and consolidated QUIC
features. `Client::with_reconnect` remains the sole native reconnect policy,
including the adapter for the unchanged foreign `once` field.

Real consumers are Tokio, relay Tokio/io_uring and ACME/HTTPS, C, UniFFI,
GStreamer, and the existing integration/benchmark clients. The foreign boolean
keylog settings still read `SSLKEYLOGFILE` at their adapter boundary. No process
environment is changed. Shared TLS policy versus inline `listen.tls` already
refused competing settings on the checked-out main, so that refusal is retained.

The driver poll guard has deterministic panic/cancellation coverage and an
existing 72-case publisher × viewer CPU sweep, including an unchanged-guard
repeat. Raw measurements are in
[`rs/moq-net/benches/transport-config`](/rs/moq-net/benches/transport-config/README.md).
They are noisy and establish no speedup.

### Remaining work

Keep the original Goal and resolve these contract boundaries before completing
this quest:

- **Zero means off:** the existing reconnect implementation deliberately floors
  retry pacing at 50ms. Its `initial` is also the healthy-session classification
  threshold, and `max` is clamped to at least `initial`. For example,
  `initial=0,max=0` still waits 50ms; `initial=1s,max=0` waits at least 1s.
  Zero `timeout` already disables the retry deadline. Do not silently describe
  this as the approved “zero always off” behavior. Decide separately whether
  zero initial disables retry waiting while preserving an independent healthy
  threshold, and whether zero max disables the cap or all retry waiting.
  Recommend preserving bounded anti-flap classification and defining these
  three meanings explicitly, then add deterministic immediate-close and GOAWAY
  loop tests. No new floor or pacing policy is approved by this checkpoint. Iroh 1.4.1
  cannot clear its default 5s connection/path heartbeat: explicit zero
  `keep_alive` is refused only at an enabled Iroh bind; other backends retain
  their supported zero behavior.
- **Iroh keylog capability:** pinned Iroh 1.4.1 offers only `Builder::keylog(bool)`
  reading a process-global environment path. Its public crypto-provider and
  external-CA APIs do not install a key logger for MoQ sessions. The checkpoint
  refuses an explicit path only when Iroh is enabled, preserving ordinary Iroh
  and native/io_uring logging. Decide whether to accept this documented
  capability loss or wait for an upstream explicit-path hook; recommend the
  hook if preserving opt-in Iroh logging is required. Do not mutate the process
  environment or introduce another boolean shim.
- **Resolver policy shape:** review the concrete
  `tls::Serve::resolver(resolver, &tls::Listen)` constructor. The policy argument
  preserves the real ACME consumers' CA roots and live pinned peers; a one-arg
  constructor would discard mTLS. Certificate/key/generate/identity sources
  alongside it are refused. Tests cover CA verification and allowlist removal
  without TLS resumption. Recommend the tested two-argument shape.
- **Shared logger shape and existing-file behavior:** review `tls::KeyLog::open`
  used by native QUIC and relay io_uring. New Unix files are mode 0600; an
  existing mode-0644 file is refused without altering permissions. This tightens
  the previous environment logger's behavior. Recommend the shared helper and
  explicit refusal, retaining the tested initialization failure and real
  handshake coverage.

After those choices, complete any resulting source/consumer changes, run the
required checks, update migration guidance to the final release-to-main
contract, and remove this quest and its references only when the Goal holds.

Public API: moq-tokio, moq-quic, moq-relay, moq-auth, kio and moq-uring breaking;
foreign records, C ABI and GStreamer property names remain unchanged. Wire: none.

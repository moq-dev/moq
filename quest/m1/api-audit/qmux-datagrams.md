# [S] QMux datagrams obey or refuse their policy

## Goal

`@moq/qmux` `Session.datagrams` is typed as the browser's
`WebTransportDatagramDuplexStream` and nothing more. Its policy fields either
take effect or refuse loudly. `incomingHighWaterMark` limits every push after
it is set. Setting `incomingMaxAge` or `outgoingMaxAge` to anything
other than `null`, or changing `outgoingHighWaterMark`, throws. Applications can
no longer construct a `Datagrams`, inject inbound packets, overwrite the
negotiated size limit, or close the receive side.

Non-goals: implementing datagram expiry or an outbound queue, and the Rust
qmux crate.

## Plan

Implementation checkpoint 2026-10-29:

- The live inbound high-water mark, unsupported-policy refusal, and DOM-only
  public/runtime view are implemented in JS QMux. The internal class has a
  private constructor and runtime construction guard; only module-private
  functions can deliver packets, update negotiated size, or end the receive
  side. No expiry or outbound-queue machinery was added.
- Reproduced the original live-policy failures before changing production
  code. All 233 QMux tests now pass, including live lower/raise behavior,
  backlog retention, draining, pending reads, cancellation, malformed policy
  input, refused setters, failure, and runtime privacy.
- Package type-check, build/publint, emitted-declaration consumer checks,
  updated API report, and the receive-backlog benchmark pass. Local
  self-review found no remaining QMux finding.
- The full `quest gate just ci` attempt failed because the local mbx Cargo
  shim reports `Argument list too long (os error 7)`. This prevents the WASM
  build selected for JS dependents and the unrelated Rust test-build lint
  regression from running. Other completed lanes passed. Do not change JS
  QMux to work around this tool-launch failure.
- Remaining: rerun the full local gate in a working build environment,
  resolve relevant failures and review findings, then delete this quest and
  its links and mark the PR ready. Keep the PR a draft until that validation
  is complete. Do not merge, enqueue, or wait for online checks or reviews.

Public API: breaking in `@moq/qmux` (the `Datagrams` export and its mutators
go, and three setters now throw). The PR's Impact bullet says: stop
importing `Datagrams` and use `session.datagrams.readable`, `.writable`, and
`.maxDatagramSize`; the removed mutators have no application replacement.
Wire: none.

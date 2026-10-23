# [M] Binding connection state

## Goal

Finish one coherent connection snapshot and revision-based waiting across the
bindings, including an immediately returned session that exposes first-dial
failure. Native/UniFFI and wrapper/OBS proposals are implemented on the draft;
settle their public shape, complete C and browser ownership, and validate the
combined result before this quest can finish. Wire formats do not change.

## Plan

The 2026-10-22 API audit approved replacing binding `status()`, `connects()`,
and both `failure()` polls with `state()` and `state_changed()`, returning the
client session immediately so first-dial failure belongs to that same state.
The obsolete Go-only accessor quest is absorbed here.

The tested native proposal carries `phase`, `connects`, `failure`, `version`,
and an opaque `revision`; `state_changed(after)` supports independent observers
and closes the gap between reading and registering. A final snapshot remains
readable, while waiting at its terminal revision ends instead of spinning.

Remaining choices and work:

- Settle lifecycle naming and the revision signature. The original `presence`
  wording is ambiguous: native `Monitor::presence` counts started and ended
  sessions, whereas Connecting/Connected/Disconnected/Migrating/Closed is a
  lifecycle phase. Recommend `phase` plus the existing connect count rather than
  exposing both counter sets without a consumer.
- Complete immediate browser dialing ownership. Browser connect still waits
  for establishment; a returned session must own its first attempt and expose
  its failure without inventing another polling surface.
- Settle C terminal observation. Its existing terminal callback removes the
  session handle. Recommend an owned terminal snapshot in the terminal event,
  preserving automatic cleanup; the alternative is an explicit caller-owned
  session lifetime. Do not implicitly retain closed handles or overload the
  existing connect-count callback.
- Finish C, all generated/wrapper/OBS integration, documentation and migration
  guidance against the settled shape, then run the final native/browser and
  cross-language checks. Regenerate Dart from combined Rust metadata when
  integrating other FFI branches; never merge generated checksums manually.

Public API: moq-ffi, moq-c, every wrapper, and OBS breaking. Wire: none.

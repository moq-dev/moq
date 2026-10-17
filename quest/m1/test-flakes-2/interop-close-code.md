# [S] Adopt reliable WebTransport refusal delivery

## Goal

MoQ carries the acknowledged close fix, and the Interop browser close-code
test reliably reports a refused session as `unauthorized`, never as
"Connection lost".

## Plan

The cause is established in [moq-dev/noq#40](https://github.com/moq-dev/noq/pull/40):
the adapter started its 100 ms force-close grace before the reliable CLOSE
capsule was acknowledged. An auth-response barrier and UDP fault gate paired
Chromium netlog with relay logs: refusal packets were still dropped when the
server sent APPLICATION CONNECTION_CLOSE, and raw `WebTransport.closed`
rejected without a local JavaScript close. The earlier HTTP/3 stream-lifetime
and client `sessionCause` fixes were already present.

The source fix waits for reliable capsule delivery before the existing
teardown grace and cancels when the connection ends. Its delivery regression
fails without the fix; its cancellation regression bounds the close task's
lifetime. Source formatting, Clippy, docs, 535 tests, and publish dry-run
passed. Full `just test interop --all` passed with an adapter-only temporary
patch, including Chromium's exact code 2 and `unauthorized`. That patch was
removed; no unpublished dependency belongs in this adoption.

- `main` already carries the fix: #5664 copied `web-transport-moq` from
  moq-dev/noq#40's merge commit into moq-tokio's `quic::web`, with its regressions
  (`close_waits_for_capsule_ack`, `close_ack_wait_ends_with_connection`).
- Run strict `just check` and `just test interop --all` on `main`,
  preserving the exact refusal assertion and the source regression. Do not
  retry for green, increase the grace, or accept alternate errors.
- Recommend a `release` backport after main lands. That branch pins the
  1.3.5 adapter, so verify its exposure and use a compatible 1.3.x source
  backport/release if needed; never move the published line to 2.x as a fix.
  Split any required source-release condition before starting its pin PR.

Public API: none expected. Wire: existing CLOSE capsule framing and codes
are unchanged; refusal delivery is corrected.

## Related

- [Source fix](https://github.com/moq-dev/noq/pull/40) - deterministic delivery and cancellation regressions, plus the paired browser/relay evidence

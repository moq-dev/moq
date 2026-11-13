# [L] A browser group stream reset reaches the reader

## Goal

`just test stream-credit` passes reliably on lite-07 in Chromium and Firefox,
and runs in the nightly interop workflow. Each group stream the relay resets
reaches the tab's reader as a reset with its code, so a YIELDED group pauses
and a subscription ends once its groups are accounted for.

## Plan

[moq-dev/moq#8652](https://github.com/moq-dev/moq/pull/8652)'s harness found
two rare failures, both where a relay's reset of a tab's group stream meets the
browser. Heavy logging in the page hides them, so reproduce with the harness as
is and add tracing that buffers in memory.

- Chromium (about 1 lite-07 cell in 20): one subscription never ends. The page
  read the header of a group the relay paused (YIELDED), continued, and then
  dropped as stale (`SUBSCRIBE_DROP`, OLD), and that stream never ended or
  errored in the page, so the lite-07 tail waits on it forever. No second header
  for the group reached the page. Chromium 156 does not negotiate
  RESET_STREAM_AT, so the relay sends a plain RESET_STREAM once the 3-byte
  WebTransport header is acknowledged (`moq_quic` `take_held_reset`). Next: log
  each group stream's ID in the relay and whether Chromium acknowledges its
  RESET_STREAM. If Chromium acknowledges it, the loss is Chromium's: report it
  upstream, and make the JS reader stop waiting on a stream whose group a drop
  or a continuation accounted for. If not, fix the relay's reset.
- Firefox (1 lite-07 cell in about 25): a few starved tracks' final groups fail
  with `TypeError: Error in input stream` and no stream error code, which the
  reader cannot tell from a YIELDED reset, so the group aborts instead of
  pausing. Firefox 157 negotiates RESET_STREAM_AT, and the relay resets with a
  Reliable Size of 3. Next: reproduce in a minimal WebTransport page, and report
  it upstream if Firefox drops the code.
- Then add `just test stream-credit` to `.github/workflows/interop.yml` after
  "Stream limits over WebSocket", and delete the epic README, whose last work
  this is.

To reproduce, loop `just test stream-credit --version moq-lite-07-wip --engine
chromium` until one fails; it keeps the run directory with the relay log, the
page console, and `pending.json`. The page's `console.debug` logs each pause.

## Related

- [moq-dev/moq#8643](https://github.com/moq-dev/moq/pull/8643) - a JS copy-fill duplicate group; the harness saw no duplicate group

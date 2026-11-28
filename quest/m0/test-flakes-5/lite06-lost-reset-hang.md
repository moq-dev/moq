# [M] A lite-06 group whose reset the browser lost never holds a reader forever

## Goal

On lite-06, a `@moq/net` reader of a group whose stream reset the browser lost (Chromium
drops a RESET_STREAM that lands just after the stream's data, before the page reads it)
gets the group ended with an error once its subscription is over, instead of a frame read
that never settles. `just test stream-credit --version moq-lite-06 --engine chromium` then
ends every subscription, run after run.

Seen once, in the Interop run [46121821106](https://github.com/moq-dev/moq/actions/runs/46121821106)
on [moq-dev/moq#9840](https://github.com/moq-dev/moq/pull/9840) with a release relay:
`FAIL 4 subscriptions still open 10000 ms after the publisher finished`, where `mid-66` to
`mid-69` each read frame 0 of group 39 and nothing more. The relay had logged `subscribed
complete` for all four and served group 39 on 300 streams while finishing 270, so it gave up
the rest, most likely by resetting them. Lite-07 copes because its publisher names every reset
group in a drop over WebTransport ([moq-dev/moq#9743](https://github.com/moq-dev/moq/pull/9743));
lite-06 has no drop. 72 other lite-06 cells on hosted runners and every scheduled run from
2026-11-22 to 2026-11-27 passed it. It recurred in scheduled Interop
[46166329015](https://github.com/moq-dev/moq/actions/runs/46166329015) (2026-11-28, `9a20cdf5bc`):
`3 subscriptions still open 10000 ms after the publisher finished`, `low-50` to `low-52` with last
groups `38:2`; its run directory is uploaded. The lost reset is still unconfirmed there.

## Plan

Confirm it is the lost reset first: with the run directory that step now uploads, add the
group stream's reset code to the relay's debug log for a failing cell, or reproduce on a
loaded host with `--version moq-lite-06 --engine chromium`. Then bound it in the lite-06 JS
subscriber. `Tail.settle` (`js/net/src/tail.ts`) returns only once no stream is being read,
even after its grace expires, so the stuck group must be found while the tail settles, not
after: once the end is declared and the grace has passed, a group stream that has delivered
nothing for that grace ends with an error the reader can tell from a clean end, so the reader
may fetch it. Test that a slow stream still delivering frames in that window finishes whole.
Lite-06 is a published wire, so the fix stays on the subscriber.

## Related

- [Chromium reset fix](/quest/m4/browser-fixes/chromium-reset-before-accept.md) - the browser bug this works around

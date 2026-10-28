# [S] js/net finishes tracks whose FIN arrives with the close

## Goal

When a peer closes a session gracefully right after finishing its tracks, a
js/net relay or subscriber finishes those tracks instead of aborting them, on
every lite version, as Rust does since
[moq-dev/moq#6741](https://github.com/moq-dev/moq/pull/6741).

## Plan

In Rust the close could reach the subscriber before it read an already
buffered SUBSCRIBE FIN, so the track ended `Unroutable`. The fix polls the
sources once after the closed transport's group streams drain. Check whether
js/net has the same ordering; if so, reproduce with a two-`Connection` test and
mirror the fix. If not, delete this quest with the evidence.

Public API: none. Wire: none.

Reproduced 2026-10-27 in
[moq-dev/moq#6969](https://github.com/moq-dev/moq/pull/6969): extending the
two-Connection final-group drain test to assert a clean terminal read fails on
lite-01 through lite-04 with `SessionCode.Cancel` after the peer's clean close.
Lite-05, lite-06, and lite-07 pass the same assertion.

The driver calls `Subscriber.close(closeError(...))` as soon as the transport
closes, before the legacy SUBSCRIBE FIN and tail completion promise chain can
finish the track. Adding an implicit `Producer.finish()` after `#settleTail`
alone does not fix the race and was discarded. The remaining work needs an
explicit, bounded drain of already-delivered group and subscribe responses
before that teardown, analogous to Rust's `poll_drain`, rather than a sleep or
extra microtask turns. The PR keeps the reproducer for that work; no
production workaround is included.

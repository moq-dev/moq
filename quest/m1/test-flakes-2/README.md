# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check` pass
reliably, each fixed at its cause, never by raising a timeout or adding a
retry.

## Plan

Decided in the 2026-09-30 audit: the round held independent flakes in one
quest, so it split into one child per flake, grouping only those that share
a fixture. Each child lands on its own.

Rules every child keeps:

- Prefer a paused clock over wall time (`moq-cli`'s subscribe tests already
  use `#[tokio::test(start_paused = true)]`), or assert on an event instead
  of a deadline. If a test is slow under load because the code under test is
  slow, fix that.
- A paused clock auto-advances while the runtime idles, so it can fire a
  timer before a real socket delivers. Where real sockets fight the paused
  clock, make the timers mockable or move the test off real sockets.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did. moq-tokio's
`a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean` is a
known exception, tracked by [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md).

Public API: none. Wire: none.

## Required

- [TS decoded content](/quest/m1/test-flakes-2/ts-video-content.md) - CI's TS compliance decoded-content checks fail only on real loss, and the exporter names every frame it drops
- [WebTransport close release](/quest/m1/test-flakes-2/interop-close-release.md) - a compatible published adapter contains the acknowledged-close fix
- [Interop close code](/quest/m1/test-flakes-2/interop-close-code.md) - adopt that release so the browser reliably sees `unauthorized` for a refused session
- [Handshake ACK](/quest/m1/test-flakes-2/handshake-ack.md) - the impaired cluster drill's connect survives a lost handshake ACK
- [Auth refresh mocks time](/quest/m1/test-flakes-2/auth-refresh-mock-time.md) - moq-relay's auth refresh and fixed-lease tests measure a grant's life on one mocked clock
- [uring teardown close](/quest/m1/test-flakes-2/uring-teardown-close.md) - moq-uring's published-close teardown test holds under load, fixed at its cause
- [Auth serve signals](/quest/m1/test-flakes-2/auth-serve-signals.md) - moq-cli's auth serve signal tests hold under load
- [Export exit tests](/quest/m1/test-flakes-2/export-failure-exit.md) - moq-cli's failed-export and interrupted-publisher exit tests hold under load
- [Session registration](/quest/m1/test-flakes-2/cli-drain-register.md) - a session the client sees established is one the CLI's drain waits for, so a SIGTERM right after the handshake still sends GOAWAY
- [noq client close](/quest/m1/test-flakes-2/noq-client-close.md) - moq-tokio's client close test orders its close after the server's accept instead of racing it

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix these rules ask for: every crate on a controlled clock
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - tracks the media fixture's cold-start audio-tone flake, a player stall it shares with the mono pin

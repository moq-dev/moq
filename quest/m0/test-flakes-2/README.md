# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check`, or
intermittently in CI or interop, pass reliably, each fixed at its cause,
never by raising a timeout or adding a retry.

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
  clock, drive the timers with a `kio::clock::Manual` clock or the sim (every
  crate's timers run on `kio::clock`), or move the test off real sockets.
- A mocked clock still flakes when the code jitters its timers with `rand`
  (one cancel race was a retry sweep jittered past a fixed settle
  budget). Wait out the jitter's whole window, not a count of small steps.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

The redundant failover drift moved to m0 on 2026-10-20 (maintainer) and is
fixed by [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113).

Added 2026-10-21 and 2026-10-22 (maintainer): failures from the m0/m1
merge runs, one child each.

Decided 2026-10-26 (maintainer): the epic moved from m1 to m0, since every
flake now fails a whole batch of the [batched merge gate](/quest/m0/ci/land.md)
rather than one PR. The queue's recurring flakes from 2026-10-23 to 10-26
were already fixed (the moq-rtsp stall tests, the moq-gst pinned-instance and
sibling-restart tests, the moq-mux missing-sequence test,
`js/common/worklet.test.ts`, and the IETF datagram range test in
[moq-dev/moq#6593](https://github.com/moq-dev/moq/pull/6593)).

The `rust-swapped -> js` and `rust -> js` browser audio timeouts were a lite-06
catalog group dropped mid-read ending the player's catalog, fixed by
[moq-dev/moq#6820](https://github.com/moq-dev/moq/pull/6820).

moq-rtc's session-end test was never load-bound: on the paused clock, dimpl's
randomly jittered DTLS handshake give-up raced ICE's establishment deadline,
fixed by [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379).

moq-relay's auth-trust test was never load-bound either: its fixture left each
lease's background `end` POST unserved, so a reused port let one steal the next
trial's accept. Fixed by [moq-dev/moq#7550](https://github.com/moq-dev/moq/pull/7550).

The Kotlin flow-cancel test's `unroutable` was not load-bound: its snapshot
writer became unreachable, so JVM cleanup aborted the track mid-read. The JS
IETF blind-handle test's one identified cause was the session-closing reset
fixed by [moq-dev/moq#6988](https://github.com/moq-dev/moq/pull/6988). A single
event-loop stall at each of its 124 host tasks passes all 124 at 1.5 seconds;
at 4.5 seconds 3 runs and at 11 seconds 114 runs hit bun's own 5-second test
timeout. No run fails on a protocol deadline, so no clock change applies. Both handled in [moq-dev/moq#7622](https://github.com/moq-dev/moq/pull/7622).

The JS close-drain fixture advanced after the setup group's first payload, so a
zero-delay subscription could expire its sender while the write budget yielded.
It now reads that group through FIN before starting the next one, fixed by
[moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658).

Public API: none. Wire: none.

## Required

- [IETF22 fallback](/quest/m0/test-flakes-2/ietf22-fallback.md) - an IETF22 browser viewer follows a fallback within the media lane's bound, keeping its session and its audio
- [Draft-22 interop with a native publisher](/quest/m0/test-flakes-2/draft22-interop.md) - no "invalid fetch range" or audio gap with a Rust or C++ publisher
- [moq-tokio's close-drain test survives load](/quest/m0/test-flakes-2/tokio-close-drain.md) - `quic_client_close_drains_finished_track` stops failing land batches with "Unroutable"
- [@moq/net's boundary-sync test survives load](/quest/m0/test-flakes-2/js-boundary-sync.md) - "a version without a boundary syncs" passes under load
- [moq-c's gated callback test frees nothing early](/quest/m0/test-flakes-2/moq-c-gated-callback.md) - the moq-c tests stop aborting with a use-after-free under load

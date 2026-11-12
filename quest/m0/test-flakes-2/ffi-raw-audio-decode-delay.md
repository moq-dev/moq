# [S] moq-ffi's raw audio decode delay test holds up

## Goal

`moq-ffi`'s `test::raw_audio_decode_delay` (feature `audio`) passes reliably,
fixed at its cause, and its `#[ignore]` is removed.

## Plan

Seen 2026-11-12 in the merge queue for
[moq-dev/moq#8525](https://github.com/moq-dev/moq/pull/8525), which does not
touch moq-ffi: [run 43198904021](https://github.com/moq-dev/moq/actions/runs/43198904021)
(job 131116860271, log lines about 6476 to 6600 of `gh run view 43198904021 --log-failed`)
aborted with SIGABRT. The previous queue run,
[43198314575](https://github.com/moq-dev/moq/actions/runs/43198314575), passed it.
A read's `JoinHandle`, awaited in the test's read loop, never returned until
the 10 s `kio::clock::timeout(TIMEOUT, ...)` fired on the host clock, which
`KIO_DENY_HOST_TIMERS=1` turns into an abort. A decoded read hanging is more
than a tight budget, so suspect a real race in the FFI audio consumer.

Hypothesis from a quick read, unconfirmed: the fixed-delay consumer asks for
`max: Some(Duration::ZERO)`. [moq-dev/moq#8050](https://github.com/moq-dev/moq/pull/8050)
found that a zero max delay makes a read that runs late under load find the
first group stale and the warmup trim all remaining audio. This test writes
only two frames and never finishes before reading, so a trimmed read would
wait forever instead of returning a frame or an end. If that is the cause,
decide whether the test should ask for no max delay, as #8050's fixed
consumer does, or whether the consumer should hold the newest frame instead of
trimming everything, rather than only loosening the delay it asks for.

Steps:

- Reproduce under load first (for example, many parallel runs of the test
  under `quest gate`, or delaying the reads past the 80 ms warmup).
- Run the test on a mocked clock, such as `start_paused` or `kio::clock::Manual`,
  as [moq-dev/moq#8329](https://github.com/moq-dev/moq/pull/8329) did for the
  warmup test, so the FFI runtime's real clock cannot skew it.
- Fix the root cause, then remove the `#[ignore]`.

Public API: none expected. Wire: none.

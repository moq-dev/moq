# [S] moq-tokio's close-drain test survives load

## Goal

`moq-tokio::integration backend::quic_client_close_drains_finished_track`
(`rs/moq-tokio/tests/integration/backend.rs`) passes reliably under a loaded
`just ci`. It's fixed at its cause, never by raising a timeout or adding a
retry.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). The test failed with "subscribe failed: Unroutable" and passed on rerun.
On 2026-11-01 it ejected two land batches, at 20:24Z and 20:44Z (see
[moq-dev/moq#7258](https://github.com/moq-dev/moq/pull/7258) and
[moq-dev/moq#7439](https://github.com/moq-dev/moq/pull/7439)), and it failed
once in [moq-dev/moq#7490](https://github.com/moq-dev/moq/pull/7490)'s full
run.

- It runs on moq-lite (`moq-lite-07-wip`, around line 801). Find the race that
  lets the subscribe reach the publisher before its route exists, or after the
  route has gone.
- Reproduce it under load first, for example by running it beside a full
  `cargo nextest`.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).

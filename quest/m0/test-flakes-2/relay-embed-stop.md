# [S] moq-relay's embedded io_uring stop test survives load

## Goal

`moq-relay::integration embed::uring_custom_route_and_quic`
(`rs/moq-relay/tests/integration/embed.rs`) passes reliably in a loaded
merge-queue `just ci`, fixed at its cause, never by raising a timeout or
adding a retry. Done when the test no longer fails the queue and a loaded
local repeat of it passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). It failed the merge-queue
batch for [moq-dev/moq#7753](https://github.com/moq-dev/moq/pull/7753)
([run 41736459964](https://github.com/moq-dev/moq/actions/runs/41736459964),
2026-11-03 23:06Z) after 0.371 s, while its `shared_tokio` and `worker_tokio`
siblings passed:

```text
FAIL [   0.371s] (7970/9724) moq-relay::integration embed::uring_custom_route_and_quic
thread 'embed::uring_custom_route_and_quic' (3744147) panicked at rs/moq-relay/tests/integration/embed.rs:51:5:
HTTP still accepted after the owner stopped
```

`assert_owner_stopped` connects to the HTTP port after the owner stops and
expects a refusal.

- Find out whether the io_uring listener was still open (the stop returned
  before its accept socket closed) or another test bound the freed ephemeral
  port in between. The relay's auth-trust flake was a reused port
  ([moq-dev/moq#7550](https://github.com/moq-dev/moq/pull/7550)).
- If the listener outlives the stop, fix the stop so it returns only after
  the socket closes. If the port was reused, prove the close another way, for
  example with the socket-inode check the QUIC half already uses on Linux.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).

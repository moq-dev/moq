# [S] A parked JS stream open gives up when its group expires

## Goal

A js/net publisher's pending stream open ends as soon as its group expires
or is evicted, as Rust's parked machines do with
[moq-dev/moq#6140](https://github.com/moq-dev/moq/pull/6140). A stalled
downstream no longer makes a JS publisher hold one pending open per group.

## Plan

Decided 2026-10-21 with the maintainer: m1. JS lite and IETF open each
group's stream with `waitUntilAvailable: false`, so an over-limit open fails
at once on a transport that rejects it. A WebTransport implementation that
parks it instead leaves one pending open per group, bounded only by the 10 s
`OPEN_TIMEOUT_MS` or the subscriber leaving.

- Cancel the pending open when its group expires or is evicted, mirroring
  Rust's `poll_abandoned`. Then decide whether `OPEN_TIMEOUT_MS` still earns
  its place, and delete it if not.
- Mocked-time test: a transport that parks opens, a group that expires while
  its open is parked, and the open released at expiry.

Public API: none. Wire: none.

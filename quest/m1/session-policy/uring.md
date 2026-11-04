# [M] The io_uring relay path uses moq-net's policy

## Goal

moq-relay serves io_uring sessions through the same moq-net accept policy and
the same `Connection::serve` and listen-config mapping as tokio, so a
lifecycle or config fix lands once. The hand-copied deadline, admission bound,
`with_deadline`, `set_limits` and drain-bounded reply in
`rs/moq-relay/src/uring.rs` are deleted.

## Plan

Part of [Session policy lives in moq-net](/quest/m1/session-policy/README.md);
see it for the decisions and research.

- Folded in 2026-11-04 (maintainer): the m2 relay-serve-path quest
  ([last version](https://github.com/moq-dev/moq/blob/e6be13d5f8/quest/m2/relay-serve-path.md)).
  Its finding stands: `uring::serve_request` re-implements
  `Connection::serve` step for step, `Workers::bind` re-implements the config
  mapping also at moq-tokio's `server.rs` and `websocket.rs`, and eight commits
  patched the drift (58eaa6afd5, 8a13d58910, 6bd1306d6e, 1f56330d21,
  d17b84ee7f, 4576bb1231, c11192c7e6, 67df70012d).
- Implement the sealed `incoming::Sealed` for a uring `Request` whose driver
  runs on the worker handle, with admission hopping to the shared runtime
  through `Lifecycle::shared`, so one `Connection::serve` serves every runtime.
  Use one listen-config mapping at all three sites.
- Do this before the io_uring packaging work publishes the duplicated path.
- The relay's existing drain, admission and handshake-deadline tests must
  pass on both runtimes.

Public API: none. Wire: none.

## Required

- [Accept policy in moq-net](/quest/m1/session-policy/accept.md) - the shared policy this path calls

## Related

- [io_uring relay package](/quest/m2/relay-io-uring-package.md) - ships the path this unifies

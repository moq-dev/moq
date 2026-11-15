# [S] The release line returns QMux receive credit

## Goal

The next `release` patch ships `qmux` 0.5.4 and `@moq/qmux` 0.3.5. Both fix
moq-dev/web-transport's receive-credit leak
([moq-dev/web-transport#507](https://github.com/moq-dev/web-transport/issues/507)).
In the leaky versions, a stream dropped or cancelled with unread data never
returns that data's connection credit (`MAX_DATA`). Once about one connection
window (1 MiB) of it has piled up, the sender cannot write on any stream of
that connection, but the session stays up and logs nothing.

On the release line that hits every QMux session in both directions:

- **Publisher to relay.** The relay cancels its upstream subscription when the
  last downstream subscriber leaves, often in the middle of a group. Each group
  stream it drops leaks its unread bytes. Eventually the publisher can send
  nothing, while the relay's requests still reach it.
- **Relay to browser.** A viewer on the WebSocket fallback cancels group streams
  it skips, so the browser's window shrinks the same way.

`main` is unaffected: its in-tree `rs/qmux` (on `moq-quic`) and `js/qmux` already
credit stopped streams, late data, and the reset gap.

## Plan

Planned 2026-11-14 (maintainer, /quest-plan in moq-dev/web-transport). Per
[RELEASE.md](/RELEASE.md), this is a backport PR onto `release` that keeps the
published API. `main` needs no change.

- On `release`, the workspace pin moves from `qmux` 0.5.3 to 0.5.4 in
  `Cargo.toml` and `Cargo.lock`. `bun.lock` moves `@moq/qmux` to 0.3.5, and
  `@moq/net` requires `^0.3.5`, so a fresh install of the next `@moq/net` can't
  resolve a leaky version.
- ✅ Done in [moq-dev/moq#8808](https://github.com/moq-dev/moq/pull/8808) on
  `release`: the pins, the `@moq/net` 0.4.8 bump (it also ships the two
  unpublished `@moq/net` backports merged since 0.4.7, #8035 and #8346), the
  audit, and the relay validation (the publisher direction fails on 0.5.3 after
  7 to 9 cancels and passes 120 on 0.5.4; the viewer direction is shown by
  `@moq/qmux`'s own credit tests, failing on 0.3.3 and passing on 0.3.5, since
  loopback never leaves data in flight after a cancel).
- ✅ Audit before cutting (maintainer, 2026-11-14). The PR audits everything the
  next release ships since each package's last published version, not only this
  bump: anything else merged to `release` since moq-relay 0.17.16 and the JS
  packages' last releases, plus the two dependency diffs. The web-transport
  release PRs audit those, so link their audits. Record the audit in the PR
  body. A finding that should hold the release goes to the maintainer before the
  release-plz PR merges.
- Validate on the built release-line relay over WebSocket, in both directions,
  against the old and new pins:
  - A publisher session gets well over 1 MiB of mid-group cancels from
    subscribers that join and leave. A later subscriber still receives groups,
    which fails on 0.5.3.
  - A JS viewer cancels groups past one window. It keeps receiving, which fails
    on 0.3.3.
  - Run the viewer past 100 cancelled streams. `@moq/qmux` 0.3.5 holds a
    cancelled stream's slot until the peer sends FIN or RESET_STREAM, and qmux
    0.5.x resets only when its app polls the stream. Group streams do that.
    Confirm the relay's other streams to the browser (announce, subscribe) don't
    sit idle holding slots.

## Waiting

- Maintainer: merge [moq-dev/moq#8808](https://github.com/moq-dev/moq/pull/8808)
  into `release` (its merge publishes `@moq/net` 0.4.8, which requires
  `@moq/qmux` `^0.3.5`), then merge the release-plz PR it triggers for `qmux`
  0.5.4's dependents (`moq-tokio`, `moq-relay`, and theirs). Check
  `npm view @moq/net@0.4.8 version` and that `cargo info moq-relay` names a
  version newer than 0.17.16. Finish this quest then by deleting it with
  `quest rm`. Not an agent step: a merge to `release` publishes.

## Related

- [Send-credit stalls](/quest/m3/send-credit-stalls.md) - names this kind of
  stall in session stats on `main`
- [Walk-away cancels](/quest/m3/walkaway-cancel.md) - the publisher logged the
  unanswerable requests as "internal error"

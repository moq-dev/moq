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

- moq-dev/web-transport: `qmux` 0.5.4 on crates.io and `@moq/qmux` 0.3.5 on
  npm. Check `cargo info qmux@0.5.4` (run outside a workspace) and
  `npm view @moq/qmux@0.3.5 version`. Their quests are web-transport's
  `quest/a0/qmux-05-recv-credit.md` and `quest/a0/qmux-js-release.md`.

## Related

- [Send-credit stalls](/quest/m3/send-credit-stalls.md) - names this kind of
  stall in session stats on `main`
- [Walk-away cancels](/quest/m3/walkaway-cancel.md) - the publisher logged the
  unanswerable requests as "internal error"

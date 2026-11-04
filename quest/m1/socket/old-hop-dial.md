# [S] A socket dial over an older hop fails instead of waiting

## Goal

A socket dial through a hop older than lite-07 fails with `Unsupported`
instead of waiting forever. Today the older hop re-mints the answer's
instance, so the dialer never sees an answer it recognizes.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1, in this epic
- m2

From [moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677), which
lands Rust sockets and documents the hang (accept already fails
`Unsupported` over an older hop).

- One option from #7677: the dialer also watches `path/.accept/<id>` and
  fails `Unsupported` on an answer whose instance `is_minted()`. Weigh the
  cost of a second announce interest per pending dial against alternatives,
  and record the choice.
- Python inherits it through moq-ffi. If [JS sockets](/quest/m1/socket/js.md)
  has landed, its dial gets the same check.
- Test through a pre-lite-07 hop: the dial fails promptly with
  `Unsupported`.

Public API: none (an existing error). Wire: none.

## Required

- [Rust sockets](/quest/m1/socket/rust.md) - the dial this bounds

# [S] Ranges refuse what the wire drops

## Goal

A subscription range with a relative or time anchor fails loud on a hop
whose upstream wire cannot carry it, instead of silently becoming live.
`floor()` honors `set_groups`.

## Plan

Decided 2026-10-22 (API audit). Keep the general range API (the
[subscribe ranges](/quest/m1/subscribe-ranges/README.md) line lands lite-07
before the cut); fix only what the audit found broken:

- `Anchor::Relative` and `Anchor::Time` are resolved at the serving hop and
  otherwise degrade to live on wires that cannot carry them (published lite
  versions, moq-transport without a local resolution). Refuse instead, with
  a test on each wire.
- `floor()` ignores `set_groups` (`rs/moq-net/src/model/track.rs`); fix and
  test.
- moq-hls `history.rs` works around a missing "unset" budget with a
  `(1 << 53) - 1` ms one; pass `None` instead, since `Subscription::max_delay`
  and the bindings' max delay are optional
  ([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)).

Public API: none. Wire: none.

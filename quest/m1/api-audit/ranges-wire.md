# [S] Ranges refuse what the wire drops

## Goal

A subscription range with a relative or time anchor fails loud on a hop
whose upstream wire cannot carry it, instead of silently becoming live.
`floor()` honors `set_groups`, and the bindings take max delay as an
optional duration before their shape freezes.

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
- Bindings expose `max_delay_us` with zero meaning the live default; take
  `max_delay: Option<Duration>` (or the binding's optional) like Rust's
  `Fetch::max_delay`. moq-hls `history.rs` works around the zero default with
  a `(1 << 53) - 1` ms budget; remove that once it can say "unset".

Public API: moq-ffi and wrappers breaking (max delay). Wire: none.

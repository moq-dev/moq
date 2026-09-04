# [S] web-transport-quinn reports the controller bandwidth estimate

## Goal

A published web-transport-quinn returns quinn's BBR bandwidth estimate from
`estimated_send_rate()`, and `None` when the controller has none, so a
consumer can tell "no estimate" from a bad one.

## Plan

The work lives in `moq-dev/web-transport`. This quest exists so the release
it produces is one condition a dependent waits on, rather than a step restated
inside the bump.

- Return `path.bandwidth_estimate` from `estimated_send_rate()` in
  `web-transport-quinn/src/session.rs`, with no cwnd/rtt fallback: CUBIC
  reports `None`, which is the honest answer.
- Tighten the `web-transport-trait` doc on `estimated_send_rate` to say the
  quantity is the congestion controller's estimate, so a third backend does
  not invent a fourth interpretation.
- Cut a web-transport-quinn release carrying it. The quest completes when
  that release is on crates.io; the bump here is
  [#2847](/quest/m2/2847-the-quinn-backends-send-bandwidth-estimate-is-cwnd-rtt.md).

## Required

- A quinn-proto release after 0.11.17 that carries [quinn-rs/quinn#2802](https://github.com/quinn-rs/quinn/pull/2802) (`PathStats::bandwidth_estimate`)

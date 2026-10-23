# [XS] The hang draft lets a dual decoder present before warmup

## Goal

`drafts/draft-lcurley-moq-hang.md` matches the shipping `decode::Consumer`:
a consumer running a parallel max-delay decoder MAY present the min-delay
decoder's pictures before the group start plus `warmup`, replacing them once
the max-delay decoder catches up. Today the draft says a consumer MUST NOT
present frames stamped before that point, while
`rs/moq-video/src/decode/consumer.rs` deliberately shows refresh stripes
after a join or skip.

## Plan

The rule was decided 2026-10-22 (maintainer) in
[Plan: recovery boundaries](/quest/m2/intra-refresh/plan-boundary.md), keeping
the dual decoder merged in [#5842](https://github.com/moq-dev/moq/pull/5842).
Split out to m1 the same day (maintainer, from an external API audit): the
decoder ships in this cut, so its contract should too. A consumer without a
second decoder keeps the MUST NOT, which is what the `consumer-warmup`
transcoder relies on. Say what a caller can rely on: pictures before warmup
may show stripes and are replaced, never that they are clean.

Publication ordering for a later, longer sweep stays in the plan quest.
js/watch runs a single decoder and today ignores video `warmup`; the
[dual decoder](/quest/m2/intra-refresh/dual-decoder.md) owns that.

Public API: none. Wire: the hang draft's presentation rule; validate with
`just drafts check`.

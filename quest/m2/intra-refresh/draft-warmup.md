# [S] The hang draft lets a dual-decoder player present before warmup

## Goal

`drafts/draft-lcurley-moq-hang.md`'s `warmup` rule permits a consumer that
runs a parallel max-delay decoder to present min-delay pictures before the
group start plus `warmup` after a non-continuous join. Today the draft says a
consumer MUST NOT, which the merged native pair in `decode::Consumer`
([#5842](https://github.com/moq-dev/moq/pull/5842)) and both player quests
violate. `just drafts check` passes.

## Plan

Split from [plan-boundary](/quest/m2/intra-refresh/plan-boundary.md) on
2026-10-22 (maintainer), so the player quests can land on a draft that
allows them without waiting on publication ordering for longer sweeps.

Keep the MUST for a consumer with a single decoder. Add the dual-decoder
exception: the min-delay pictures MAY be presented while a max-delay decoder
that joined at least `warmup` earlier converges, and the max-delay picture
takes over once its timestamp reaches the min-delay one. Record the change in
the draft's changelog.

Public API: none. Wire: none (draft text only).

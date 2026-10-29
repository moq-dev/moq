# [XS] Lite fetch backstop

## Goal

The pre-lite-05 fetch fills a lite subscriber holds (`FeedFetches` in
`rs/moq-net/src/lite/subscriber.rs`) are bounded by design, or by the
session's configured limits, not by `Limits::default().announces`.

## Plan

The implementation removes the unrelated announcement backstop. `FeedFetches`
holds at most one entry per existing model fill; overlapping readers share
fills, range width does not allocate one entry per group, and cancellation
wakes the holder to release unused fills on its next poll. Network demand
belongs to downstream request streams and their transport credit or legacy
request window, while local callers own their demand handles. An upstream
track aggregates that demand, so its announcement allowance is not the bound.

Remaining before completion:

- Run the new shared-demand/wide-range test and the existing feed tests.
  The attempted pre-fix reproduction could not start: `quest gate` waited
  over ten minutes for host admission, then was stopped before compilation.
- Run `just ci`, resolve any findings, then delete this quest and its links
  and mark the PR ready. Formatting and quest validation can run without
  waiting for host admission.

Public API: none. Wire format: none. The legacy-only runtime refusal is removed.

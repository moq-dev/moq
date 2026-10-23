# [S] JS resume reports drops

## Goal

The package-internal `hooks.tryRecvGroup(subscriber, true)` on a routed
(resumed) `@moq/net` `Track.Subscriber` reports the groups its copies gave up as a `drop`, as a
direct subscriber does and as the Rust resume reader forwards its serving
copy's `poll_drop`. A lite or IETF publisher serving a routed track then sends
`SUBSCRIBE_DROP` for them instead of leaving its peer waiting on groups that
never arrive.

## Plan

Found by Codex on [moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138)
and present on `main` before it: `#tryRecvGroup` hands a resumed subscriber to
`#tryResumed`, and `resume.ts` `take()` reads each copy without `drops`, so an
upstream `SUBSCRIBE_DROP` or a budget skip on a copy is consumed silently.

Open decisions, to settle before implementing:

- Which copy reports drops across a switch. Recommended: the serving copy
  only, as Rust does in `model/resume.rs`.
- Whether a group that failed on one copy but that another copy may still
  deliver counts as a drop. Recommended: no, until no copy can deliver it.
- How a reported run joins the resumed reader's delivered record, so a later
  copy never serves a group already reported dropped.

Regression test: a resumed subscriber whose serving copy receives
`SUBSCRIBE_DROP` reports it through `hooks.tryRecvGroup(subscriber, true)`, and a
downstream lite-07 publisher forwards it.

Public API: behavior only. Wire: none.

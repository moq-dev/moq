# [M] Apply draft 14-16 request updates in Rust and JS

## Goal

On moq-transport drafts 14 to 16, a SUBSCRIBE_UPDATE or REQUEST_UPDATE that
reaches its target request is applied, and answered where the draft requires,
in both Rust and `@moq/net`, without ending the request or leaking it.

## Plan

#4961 (Rust) routed updates to their target and taught the d14-16 watchers to
read framed messages: an update applies `subscriber_priority`, d15/16 answer
it with REQUEST_OK or REQUEST_ERROR, and a refused d14 update ends the
subscription with INTERNAL_ERROR. Remaining (maintainer, 2026-10-07, from
#4961 and #5011):

- JS: #5011 routes updates to their target, but the d14-16 publisher never
  reads the subscribe stream again, so an unread update keeps the stream from
  reporting closed and a later UNSUBSCRIBE is lost. Read framed messages and
  apply updates as Rust now does. Rebase #5011 onto that or fold it in.
- Drafts 15 and 16: an update aimed at a namespace or fetch request is
  skipped with no REQUEST_OK or REQUEST_ERROR; answer it.
- Draft 14: every field is mandatory, so a narrowed start or end group looks
  like a repeat and is ignored; decide whether to apply it.

Test: an update keeps a live subscription open and a later UNSUBSCRIBE still
ends it, on d14, d15 and d16, in both languages.

## Related

- [JS group handover](/quest/m1/js-group-handover.md) - JS also lacks Rust's route-failure splice

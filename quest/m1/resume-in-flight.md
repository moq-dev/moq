# [M] A replaced copy stays while a group below its bound is in flight

## Goal

After a same-epoch route swap, a replaced copy bounded below the new copy's
first group stays subscribed until every group under that bound it may still
deliver has arrived, been dropped by its publisher, or fallen outside the
reader's budget. An arrival-order reader then gets a group the old route was
still sending when the new route took over, in Rust and JS alike.

## Plan

Found by a hosted Codex review of #5836. The new copy handing out group F
bounds the old copy at F-1, and the old copy is then kept only while a group
under the bound is *buffered* unread: JS `Reader.#letGo` in
`js/net/src/resume.ts` asks `hooks.hasUnreadGroup`, and Rust `Reader::let_go`
in `rs/moq-net/src/model/resume.rs` asks `has_unread_group`. A read before
F-1's stream header lands finds nothing buffered and lets the copy go, so F-1
never arrives. Main bounds the old copy at the swap itself and never delivers
that group in either order, so this completes #5836's handover rather than
fixing a regression. An ordered reader past F already skips F-1, so only
arrival order gains.

- Keep a bounded copy while a gap under its bound is still open, using the rule
  [Resume reorder](/quest/m1/resume-reorder.md) settles for a passed group (a
  gap stays open for the subscription's grace) rather than a second rule.
- The copy goes once its group channel ends, every sequence under the bound has
  arrived or been dropped, or the reader's budget has expired the gap. Never
  wait on a peer past that: relays report what they have.
- Regression test in both languages, with mocked time: the old copy writes F-1
  only after the new copy handed out F and the reader polled, and an
  arrival-order reader still gets F-1. A dropped or expired F-1 lets the copy go.

Public API: behavior only. Wire: none.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - the gap rule this reuses
- [Rust resumed reads](/quest/m1/resume-ordered.md) - another resume follow-up from #5836

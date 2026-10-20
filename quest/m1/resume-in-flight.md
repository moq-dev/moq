# [M] A replaced copy stays while a group it may still carry is needed

## Goal

After a same-epoch route swap, a replaced copy is let go only once nothing it
may still carry is needed, in Rust and JS alike:

- A copy bounded below the new copy's first group stays subscribed until every
  group under that bound it may still deliver has arrived, been dropped by its
  publisher, or fallen outside the reader's budget. An arrival-order reader then
  gets a group the old route was still sending when the new route took over.
- A copy that receives a handed-out group after hand-out backs that group as if
  it had held it then, so a later failure of the serving copy's group continues
  from it.

## Plan

Found by hosted Codex reviews of #5836.

**In flight below the bound.** The new copy handing out group F bounds the old
copy at F-1, and the old copy is then kept only while a group under the bound
is *buffered* unread: JS `Reader.#letGo` in `js/net/src/resume.ts` asks
`hooks.hasUnreadGroup`, and Rust `Reader::let_go` in
`rs/moq-net/src/model/resume.rs` asks `has_unread_group`. A read before F-1's
stream header lands finds nothing buffered and lets the copy go, so F-1 never
arrives. Main bounds the old copy at the swap itself and never delivers that
group in either order, so this completes #5836's handover rather than fixing a
regression. An ordered reader past F already skips F-1, so only arrival order
gains.

- Keep a bounded copy while a gap under its bound is still open, using the rule
  [Resume reorder](/quest/m1/resume-reorder.md) settles for a passed group (a
  gap stays open for the subscription's grace) rather than a second rule.
- The copy goes once its group channel ends, every sequence under the bound has
  arrived or been dropped, or the reader's budget has expired the gap. Never
  wait on a peer past that: relays report what they have.

**Late backups.** A handed-out group leases the other copies holding it as
backups, but only those holding it at hand-out (JS `Reader.#handOut` via
`holding`, Rust `Reader::hand_out` via `peek_group`). A replaced copy that gets
group F afterwards holds no lease, and F is above its F-1 bound, so the next
poll lets it go. A failure of the serving copy's F then waits for another
route, though an identical whole copy was there.

- Decide between leasing late arrivals while a handed-out group is out (the
  reader knows which groups are out and can lease a copy when one lands) and
  keeping every replaced copy until the groups handed out above its bound end.
  Prefer whichever keeps one lease rule for both paths.

**A clean end with a copy still retained.** When the new copy B reaches clean
EOF before delivering any group, JS `Reader.#end` (in `js/net/src/resume.ts`)
and Rust `Reader::poll_end` treat the serving copy's clean end as the track's
end at once. The old copy A, still unbounded because B never delivered, may yet
deliver a final group, and that group is lost once `recvGroup()` has returned
`undefined`.

- Defer the clean end until every retained copy has drained or been bounded,
  under the same gap rule as above, so the end never cuts off a group a kept
  copy still carries.

**Bounded retention.** A reader that never reads groups (datagrams only) keeps
every bounded replaced copy that still has an unread group, since nothing ever
reads it. Rust `Reader::let_go` does this on main too. A peer that keeps
migrating the route then leaves one subscription and cache per swap. Keeping
copies longer, as above, must not make that worse.

- A reader may read groups later (`datagram_reads_keep_unread_groups_on_a_replaced_copy`),
  so an unread group still keeps its copy. Bound that by what the reader could
  still take: a copy goes once its unread groups fall outside the reader's
  budget. Decide what an unbounded budget keeps, for example the unread
  backlog consolidated into one retained cache instead of one copy per swap,
  rather than a count cap.
- Regression test: a datagram-only Rust reader across many swaps, each copy
  holding an unread group, keeps retained copies bounded by its budget.

**Datagrams read before groups.** JS errs the other way. The subscriber's
`#tryDatagram` in `js/net/src/track.ts` passes `delivered` to
`Reader.tryDatagram` only once `#groupsRead` is set, and `Reader.#letGo`
drops every replaced copy when `delivered` is undefined. A `recvDatagram()`
after a swap but before the first `recvGroup()` therefore lets the old copy go
with an unread group the new copy lacks, and that group is never delivered.
Rust keeps it (`datagram_reads_keep_unread_groups_on_a_replaced_copy`).

- Pass `delivered` regardless of `#groupsRead`, so a datagram poll asks
  `hasUnreadGroup` like a group poll does. The budget rule
  above then bounds what a reader that never reads groups keeps, in both
  languages alike.
- Regression test: port the Rust test to `js/net/src/resume.test.ts`, polling
  `recvDatagram()` between the swap and the first `recvGroup()`, which must
  return the old copy's group.

Regression tests in both languages, with mocked time:

- The old copy writes F-1 only after the new copy handed out F and the reader
  polled, and an arrival-order reader still gets F-1. A dropped or expired F-1
  lets the copy go.
- The old copy finishes F after the new copy handed F out and the reader polled
  on. The serving copy's F then fails, and the read continues from the old copy.
- The new copy ends cleanly before delivering a group, the old copy then writes
  its last group, and the reader gets that group before the end.

Public API: behavior only. Wire: none.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - the gap rule this reuses

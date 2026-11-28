# [M] An ordered front read waits for a group a replaced copy lost

## Goal

An ordered read of a logical track a front serves (`rs/moq-net/src/model/resume.rs`,
`js/net/src/resume.ts`) hands out what a plain ordered cursor would: a group below the serving
copy's resume point that a replaced copy held and then lost from its cache is asked of the serving
route and waited for within the budget, not passed over by the next group above it.

## Plan

Found by [moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750)'s review (round 3,
"Recheck lower drains before advancing an ordered reader"). The same behavior is on `main` at
065a4c1843, so #9750 did not cause it.

Repro (Rust, both copies `received_copy()`, budget an hour): write 0 on A and read it ordered,
write 1, 2, 3 on A, switch to B (resume point 3, so the walk leaves 1 and 2 to A to drain), take 1
ordered, evict 2 from A, then read ordered. The read hands out 3, B is never asked for 2, and 2
written on B later is passed. An unordered read asks B for 2 and hands it out. Evicting 2 from A
before the switch gives the same ordered result.

Cause: once A lost 2, A's ordered cursor offers 3. Rechecking A's drains below 3 before the
hand-out re-arms the walk, which asks B for 2 with a backfill copy, but nothing holds an ordered
candidate above a backfill still out below it, so 3 goes out and passes 2 for good. Tried in #9750:
a `below` bound on `Reader::rescan_drains` (pop handed-out keys from the bottom of `Copy::drains`,
check only keys under the candidate, so it costs the groups handed out since) asks B for 2 but
still hands out 3 first.

- Hold an ordered candidate while a copy asked for a key in the delivered gaps below it may still
  land that key, as `Reader::owes_below` does for one copy's own holes, within the read's budget.
- Recheck drains below the candidate before an ordered hand-out (the `below` bound above) so the
  backfill is asked before the hold judges it.
- Mirror both in JS `tryGroup`, and test the repro above in Rust and JS.

Public API: none. Wire: none.

# [M] An appended group wakes only the parked reads it expires

## Goal

A group appended to a track wakes only the parked group serves whose expiry
it crosses, not every parked serve. A publisher sending one 2.5 ms Opus frame
per group to a subscriber with a 2 s max age (about 800 parked serves) keeps
its runtime thread near idle, and `just test interop --all` passes its
Python and Go publisher rows again. A group held across a route switch is
woken when it goes stale, not at the next unrelated append.

## Plan

Root cause, found 2026-10-06 while chasing the red Interop nightly: since
#4162 raised the browser's audio max age to 2 s, an FFI publisher's
subscription parks up to ~800 group serves. Every parked serve registers on
track-state changes and on the next unstamped group past the edge
(`GroupExpiry::is_expired` in `rs/moq-net/src/model/track.rs`, reached from
`group::Consumer::poll_expired_while_pending` in the lite and IETF
publishers' `GroupServe`). Each append wakes all N, so the single moq-ffi
runtime thread sits at ~110% CPU, QUIC idles out, and subscribers see
"internal error" or RESET_STREAM. #4892 made each check O(log N) but kept the
N-wide wake per append. It depends on load and build profile, so it fails on
unrelated branches.

Decided 2026-10-06: deadline-indexed wakes. A parked read goes stale only
once the live edge passes its reach plus its budget, so keep parked reads in
a track-level map ordered by that point and wake only those the advancing
edge crosses. Keep the register-before-judge order that prevents lost wakes.
Rejected: skipping expiry checks while a serve waits for stream credit
(stale serves linger, lost-wakeup risk, fan-out stays elsewhere), capping
backlog serves per subscription (changes delivery scheduling), and lowering
the browser audio max age or using 20 ms frames in the interop clients
(hides the bug).

Decided 2026-10-06, folding in #4950: the index has a second caller.
`Recover::poll` in `rs/moq-net/src/model/resume.rs` gives up a held group on
an old route through `Consumer::poll_stale`, which registers only on the
track state, so its successor's first timestamp (or abort, or a newer
group's first timestamp moving the edge) never wakes it. It is judged with
`drifted` against a copy that usually lacks the group, and its reach ends
where the successor's first timestamp lands. Recover registers the held
group in the same index as `GroupServe`, and stamping or aborting the
successor re-keys the entry. One mechanism, not a per-reader helper beside
the index. Two differences the index must carry: an entry can wait on its
successor's first stamp with no deadline yet, and Recover's entry lives in
the serving copy's index (a different track from the one holding the
group), so it moves when the route changes.

Tests:

- A mocked-time regression test that counts wakes: N parked serves, one
  append, and only the crossed serves wake. It fails today.
- Sweep `track_parked_read` in `rs/moq-net/benches/track.rs` over parked
  readers, so a per-append cost that grows with N shows up as a slope.
- `just test interop --all` passes the Python and Go publisher rows on CI.
- #4950's `a_group_no_route_continues_wakes_when_its_successor_is_stamped`
  in `resume.rs`: the successor's first timestamp wakes the held group,
  which then reads `Error::Old`. It fails today; extend it to the successor
  aborting and a newer group stamping the edge.

Public API: none. Wire: none.

## Closes

- [#4950](https://github.com/moq-dev/moq/issues/4950) - a group held across a route switch is not woken when its successor gets its first timestamp

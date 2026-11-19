# [M] Nightly races the route layer across threads

## Goal

Nightly runs a seeded stress of a publisher, a relay, and readers over mock
moq-lite-07 sessions on a multi-threaded runtime, mixing the steps whose
order two tasks decide: a broadcast ending as another starts on the same
session, a request arriving as its route lands, a grant widening, and a
session closing during an edit. A reader that hangs, or a request refused
for a claim the reader was shown, fails the run with its seed, its round,
and the steps in flight. PRs run a short fixed count.

## Plan

Decided 2026-11-18 (maintainer, in the coordinating session): ✅ planned
(rejected: leaving it out).

[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) put a claim in
front of a reader before its route reached the origin, so a request could be
refused as unroutable. Every route test scripts its order on one thread, and
only the interop `finish` lane saw it, intermittently.
[moq-dev/moq#9052](https://github.com/moq-dev/moq/pull/9052) found it with a
scratch stress that was not kept: one publisher, one relay, and 8 readers,
ending one broadcast and announcing another on the same session. It failed
within 72 to 827 rounds before the fix and ran 18,000 clean after.

Agent's choices, for the implementer to revisit:

- m3: it tests code, not the wire, so a bug it finds after the cut is a
  patch release.
- Show it has teeth: with #9052's fix reverted it fails inside the nightly
  budget. Measure the rounds that takes and report it.
- Real threads do not replay from a seed. Try a controlled scheduler first,
  such as `shuttle`, if the mock sessions and `kio` run under one; otherwise
  log the steps in flight so a failure can be read without a replay.
- [Random fault schedules](/quest/m4/cluster-fault-schedules.md) searches
  fault timings on the simulated clock and may come to cover this. Say in
  the PR which orders it cannot reach.

Public API: none. Wire: none.

## Related

- [Random fault schedules](/quest/m4/cluster-fault-schedules.md) - the simulated search this sits beside

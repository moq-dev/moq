# [M] A refused idle copy withholds its cache at every hop

## Goal

Behind any chain of relays, a returning reader never gets a restarted
publisher's old group. An idle copy that its upstream refuses before
answering, or answers below its cache, withholds its cache and ends, and no
hop answers from a withheld cache. Covers lite-07 and moq-transport; lite-05
and 06 carry no largest position and keep today's behavior.

Today only the relay next to the worker catches the restart (#5057). With an
edge relay in between, the returning viewer still gets the old group on every
version: the edge's idle copy is refused, aborts, and an aborted copy lets its
readers drain the old cache.

## Plan

Facts from #5057's head a2723ded3 (line numbers at time of writing):

- lite: on lite-05+ the upstream's reset lands in `ServeLoop::poll`
  (`lite/subscriber.rs` ~4634) as `ServeEnd::GiveBack(err)`, and
  `TrackServeRun` calls `serving.abort(err)` (~4196). An aborted copy is
  readable (`TrackState::readable`, `model/track.rs:488`), so readers drain
  the idle cache. `Unroutable` is not in `route_failed` (`resume.rs` ~214).
- moq-transport: the middle relay waits on `track.poll_live`
  (`ietf/publisher.rs:684`), but its own copy's abort unblocks that wait, and
  `live_edge` (~7187) reads `latest()` and `peek_latest`, which ignore the
  `live_floor` that `withhold_cache` raises. Its SUBSCRIBE_OK carries the
  hidden group as Largest, so the edge sees no regression, goes live on its
  old group, and then gets PUBLISH_DONE `InternalError` (~819), which aborts
  its copy without closing the source (`ietf/subscriber.rs` ~2205). The
  error-answer branch (~1999) also aborts without withholding.
- Every source of a refusal toward a rejoining copy today: no covering route
  (`origin.rs` ~2440-2520, ~2722), a lite-07 epoch mismatch (`origin.rs:3843`),
  supersede (`front.rs:676`, removed by
  [Restart](/quest/m0/broadcast-epoch/restart.md)), and #5057's regression.
  On the wire they can't be told apart: lite resets with 0x36, and
  moq-transport sends DOES_NOT_EXIST, read back as `NotFound`
  (`ietf/error.rs` ~304, ~330).
- lite-07: a group header (`lite/subscriber.rs:823-833`) or a datagram
  (~473-478) makes the copy live before START, after which `idle_cached()`
  returns `None` and the regression check is skipped. A lite-07 publisher
  always writes START before the first group (`start()` ~2645).

Decided 2026-10-08:

- An idle copy refused before it was answered can't judge its cache, so it
  withholds it, whatever the error. A verdict error closes the source so the
  front ends (#5054's guarantee); a `route_failed` error fails over as today.
  This is right for every refusal source above, and needs no wire change.
  Rejected: matching UNROUTABLE only, which moq-transport can't carry
  distinctly, and a new regression error code, a wire change that doesn't
  help moq-transport.
- No hop answers from a withheld cache: `live_edge` honors the withheld
  floor, and a moq-transport relay whose copy ended refuses the downstream
  SUBSCRIBE instead of sending SUBSCRIBE_OK with the hidden group.
- lite-07 defers going live on a group until START. A datagram-only track
  never gets a START, so it keeps the gap, which needs a reader starting
  below the cache; document it as a known limit.
- [Claim-served epochs](/quest/m0/broadcast-epoch/claim-epochs.md) requires
  this quest: its refused-epoch front end is the same handler.

Verification: mocked time, a viewer behind an edge relay and the relay next
to a claim worker that restarts at group 0 within the linger, on lite-07 and
moq-transport 14, 16 and 22: the returning viewer gets an error and then the
new output from group 0, never the old group. A lite-07 case where a group
stream arrives before START. A same-epoch standby still resumes, and a
`route_failed` refusal still fails over.

Public API: none expected. Wire: none new.

## Related

- [Relay largest](/quest/m1/ietf-cold-largest.md) - removes the false positive where a relay under-reports its upstream's largest
- [Restart](/quest/m0/broadcast-epoch/restart.md) - removes supersede as a refusal source

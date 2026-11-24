---
name: verify-resume
description: Model-check logical track resume across route switches (`rs/moq-net/src/model/resume.rs`) with TLA+. Use when changing switch boundaries, copy capping, backfill, copy retention, or track-end ordering.
---

# Verify track resume

`rs/moq-net/verify/resume/ResumeReader.tla` models one `Reader` of a logical track that `resume::Producer` serves across route changes. Each route is a received track whose groups land in any order, even below an end its session already settled. The front serves one route at a time, switches to another live route at will, and ends the track cleanly once the serving route completes, or unroutable once no route is left. Each copy's cursor holds what landed in its window and picks the newest first, as an arrival read in the default Group Order does, and a poll takes the newest any copy offers, a tie going to the newer copy. One reader poll (`poll_first`, through `sync` and `backfill_gaps`) is one atomic step, as the reader's lock makes it; so is a switch, which the reader follows at once as `serve_from` does. TLC checks every ordering of arrivals, route failures, switches, polls, and finished reads under the bounds in each config.

## Rules

Each rule is an INVARIANT, except `SwitchCapsAtBoundary`, a PROPERTY over each switch step; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `OnceEach` | A group is handed out at most once, whichever copies deliver it. | A viewer plays a group twice after a failover. |
| `CleanEndIsComplete` | A clean end comes with every group the serving route held once complete, every group all live routes hold, and every group all live routes held as it ended, late ones included. | The track ends cleanly with a hole a route could still fill, or one a group in flight already filled; the viewer loses media silently. |
| `EndWaitsForReplaced` | The serving copy draining ends the track only once no copy it replaced may still deliver a group the reader lacks below that end. | A group still in flight on the old route is lost at the end. |
| `CleanEndIsDeclared` | A clean end only follows a route declaring one. | A failed route reads as the broadcast ending. |
| `FailsOnlyUnroutable` | The track fails only once the front gave up: a route failing is the front's to replace. | A single relay failure ends playback that another route could carry. |
| `QuietIsSettled` | Once the serving route is complete and the reader holds every group, a poll ends the read rather than parking. | A finished track hangs open on a replaced route. |
| `ReplacedCapped` | A switch caps every copy it replaces at the boundary at once: only the serving copy is unbounded. | The old route keeps sending groups the new one also sends, paying twice for every group after a failover. |
| `SwitchCapsAtBoundary` | Each copy a switch replaces is capped at most at the boundary the switch split at, as the reader stood when it switched. | A cap past the boundary lets the old route send a group the new one also sends. |
| `CopiesBounded` | However often routes flap, a route keeps at most its own copy and one backfill of it subscribed, plus one backfill for each route that failed while it still had groups to drain. | Each flap leaks a subscription upstream. |

## Run

```sh
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla -config rs/moq-net/verify/resume/ThreeRoutes.cfg
```

The first checks every rule over two routes, three groups, two switches, and a reader holding each group until it finishes it: about 18.7M distinct states, in about seven minutes. The second trades a group for a third route, so a route can come up after another failed: about 4.9M states, in about a minute and a half. Both pass at the sync commit. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Open findings

None at the sync commit.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path as a test in `model/resume.rs`'s `mod test`, one call per step: `received_copy` for a route, `routes.serve(copy.consume())` for a switch, `write` for a landing group, `finish()` for a declared end, `abort(..)` for a route failing, and `drain` or `poll_recv_group` for a poll. Pin a path the code follows as a `residual_*` test with its own config, marked with an `\* Open finding:` line, so the others still pass.

## Coverage

Sync commit: `8c939bc9d05e5e882fb4e042a9efbf2db227b721`, plus [moq-dev/moq#9103](https://github.com/moq-dev/moq/pull/9103), which reads a group landing late on a drained copy, and [moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276), which splits a switch at one boundary. Run `git log 8c939bc9d0.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `boundary`, `Copy::cap` (reuse of a route's copy, new copy at the boundary, replaced copies capped there) | `Sync`, `Boundary`, `Cap`, `Serve` |
| `rs/moq-net/src/model/resume.rs` `Reader::backfill`, `backfill_gaps` | `Backfill`, `BackfillGaps` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_first`, `deliverable`, `take`, `Copy::done` | `Poll`, `Pass`, `Scan`, `Peek`, `Deliverable`, `Take` |
| `rs/moq-net/src/model/track.rs` `Cursor::poll_recv_with` (the pick in Group Order) | `Peek` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains` | `LetGo`, `Retains` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_end` | `PollEnd` |
| `rs/moq-net/src/model/resume.rs` `Producer::splice`, `serve_from`, `Producer::end`, `Phase::Finished` | `Serve`, `FrontFinish`, `FrontFail` |
| `rs/moq-net/src/model/track.rs` `TrackState::is_complete`, `may_land`, `max_key`, `Consumer::latest_key`, `Consumer::poll_arrived`, `track::Subscriber::has_unread_group`, `Producer::finish_shared` | `Seal`, `Held`, `MayLand`, `Latest`, `Landed`, `HasUnread`, `Settle` |

## Not covered

Whole groups only, so no frames and no `Recover`: a group whose copy stalls or fails mid-read continuing on another copy is left out, as are compressed groups and fetches. The subscription is unbounded from the oldest group with an hour's budget, so nothing goes stale or expires (nor is the budget clock a replaced copy runs on, `since` and `drained`, modeled), and local ranges (`set_range`, a lowered floor's backfill), ordered reads, ascending order, caps, refusals, drops and their reports, and re-offers are left out. Copies answer at once (no `Sub::Pending`), the reader joins before any group lands, a route declares its end only once it holds the last group, and a route that declared its end never fails. A pass covers two routes with three groups or three routes with two, two switches between live routes, and one reader. `resume_model.rs`'s proptest and the tests in `resume.rs` own the rest.

---
name: verify-resume
description: Model-check logical track resume across route switches (`rs/moq-net/src/model/resume.rs`) with TLA+. Use when changing switch boundaries, copy capping, backfill, copy retention, or track-end ordering.
---

# Verify track resume

`rs/moq-net/verify/resume/ResumeReader.tla` models one `Reader` of a logical track that `resume::Producer` serves across route changes. Each route is a received track whose groups land in any order, even below an end its session already settled. The front serves one route at a time, switches to another live route at will, and ends the track cleanly once the serving route completes, or unroutable once no route is left. Each copy's cursor holds what landed in its window and picks the newest first, as an arrival read in the default Group Order does, and a poll takes the newest any copy offers, a tie going to the newer copy. One reader poll (`poll_first`, through `sync` and `backfill_gaps`) is one atomic step, as the reader's lock makes it; so is a switch, which the reader follows at once as `serve_from` does. TLC checks every ordering of arrivals, route failures, switches, polls, and finished reads under the bounds in each config.

`rs/moq-net/verify/recover/StaleRecover.tla` models one group a reader reads from a copy whose upstream subscription resets the group's stream, or drops the group, as too old (`OLD`). The relay's demand retunes the copy's budget at will, the publisher may judge under any budget asked so far, and the reader, at the live edge, wider, or bounded like a lite-07 fetch, either takes the verdict or asks the route under its own terms. It checks the cache rule in `AGENTS.md`: what a reader gets never depends on what the copy's own subscription asked.

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
| `AnswersAsUpstream` (StaleRecover) | A copy's too-old verdict ends a read only when the reader's own request would be answered too old now. | A fetch or a wider reader loses a group upstream still sends, because the relay's own subscription asked a narrower budget. |
| `AsksOnlyWhenWider` (StaleRecover) | A reader asks the route again only when it is wider than the narrowest budget the copy asked. | Every live-edge reader of a stale group costs the route a request that can only be refused. |

## Run

```sh
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla -config rs/moq-net/verify/resume/ThreeRoutes.cfg
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla -config rs/moq-net/verify/resume/Evict.cfg
```

```sh
quest gate just rs tla moq-net/verify/recover/StaleRecover.tla
```

The first checks every rule over two routes, three groups, two switches, and a reader holding each group until it finishes it: about 20.3M distinct states, in about eight minutes. The second trades a group for a third route, so a route can come up after another failed: about 5.0M states, in about a minute and a half. The third lets the cache of a route no longer served evict one group, so a group a switch left a replaced copy to drain is lost there and asked of the serving route after all: about 14.7M states, in about four minutes. With `ReArm = FALSE` it fails `CleanEndIsComplete` in seconds. All three pass at the sync commit. StaleRecover checks two budgets the copy retunes between and three readers in under a second; with `Rule = "stand"` it fails `AnswersAsUpstream`, with `Rule = "ask"` `AsksOnlyWhenWider`, and with `Recorded = "current"` `AnswersAsUpstream`. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Open findings

None at the sync commit.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path as a test in `model/resume.rs`'s `mod test`, one call per step: `received_copy` for a route, `routes.serve(copy.consume())` for a switch, `write` for a landing group, `finish()` for a declared end, `abort(..)` for a route failing, and `drain` or `poll_recv_group` for a poll. Pin a path the code follows as a `residual_*` test with its own config, marked with an `\* Open finding:` line, so the others still pass.

## Coverage

Sync commit: `c53243d095730a38dd51110d66920a70f09a9a43`, plus the re-armed backfill of a group a draining replaced copy's cache evicts (`Copy::drains`, `Evict`). Run `git log c53243d095.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/track.rs` to see what changed since. StaleRecover's rows sync to `541c0839bfdcf0050b23ca217c4b35efa51941f3` plus the copy's judging budget (`judge_under`, `judged_under`) and `Recover::outgrows_judging` that model it. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `boundary`, `Copy::cap` (reuse of a route's copy, new copy at the boundary, replaced copies capped there) | `Sync`, `Boundary`, `Cap`, `Serve` |
| `rs/moq-net/src/model/resume.rs` `Reader::backfill`, `backfill_gaps`, `Copy::drains` | `Backfill`, `BackfillGaps`, `Scan` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_first`, `deliverable`, `take`, `Copy::done` | `Poll`, `Pass`, `Scan`, `Peek`, `Deliverable`, `Take` |
| `rs/moq-net/src/model/track.rs` `Cursor::poll_recv_with` (the pick in Group Order) | `Peek` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains` | `LetGo`, `Retains` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_end` | `PollEnd` |
| `rs/moq-net/src/model/resume.rs` `Producer::splice`, `serve_from`, `Producer::end`, `Phase::Finished` | `Serve`, `FrontFinish`, `FrontFail` |
| `rs/moq-net/src/model/track.rs` `TrackState::is_complete`, `may_land`, `max_key`, `Consumer::latest_key`, `Consumer::poll_arrived`, `track::Subscriber::has_unread_group`, `Producer::finish_shared` | `Seal`, `Held`, `MayLand`, `Latest`, `Landed`, `HasUnread`, `Settle` |
| `rs/moq-net/src/model/track.rs` `TrackState::evict_group`, `evicted_groups`, `Consumer::evicted` | `Evict`, `Evicted`, `Arrived` |
| `rs/moq-net/src/model/resume.rs` `Recover::undelivered`, `outgrows_judging`, `poll_serving` (StaleRecover) | `Recover` |
| `rs/moq-net/src/model/track.rs` `Producer::judge_under`, `Consumer::judged_under`, `settles` (StaleRecover) | `Retune`, `Reset`, `Recover` |
| `rs/moq-net/src/lite/subscriber.rs` `SubStream::stale_budget` (StaleRecover) | `Reset` (`Recorded`) |

## Not covered

StaleRecover covers one group, one route, and a publisher that holds the group for every request; it leaves out frames, caps, and route changes, which the tests in `resume.rs` own.

ResumeReader: whole groups only, so no frames and no `Recover`: a group whose copy stalls or fails mid-read continuing on another copy is left out, as are compressed groups and fetches. The subscription is unbounded from the oldest group with an hour's budget, so nothing goes stale or expires (nor is the budget clock a replaced copy runs on, `since` and `drained`, modeled), and local ranges (`set_range`, a lowered floor's backfill), ordered reads, ascending order, caps, refusals, drops and their reports, and re-offers are left out. Copies answer at once (no `Sub::Pending`), the reader joins before any group lands, a route declares its end only once it holds the last group, and a route that declared its end never fails. Only a route no longer served evicts, and a group a route evicted is owed only by the routes still holding it, since the fill a cursor of its own would ask for it is left out. A pass covers two routes with three groups or three routes with two, two switches between live routes, and one reader. `resume_model.rs`'s proptest and the tests in `resume.rs` own the rest.

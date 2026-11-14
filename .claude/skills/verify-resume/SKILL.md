---
name: verify-resume
description: Model-check how a front's reader resumes one logical track across route changes (`rs/moq-net/src/model/resume.rs`) with TLA+. Use when a change touches `Reader::sync`, a copy's bound (`observe`, `bound_at_end`), backfill (`backfill`, `backfill_gaps`), which replaced copies are kept or waited on (`let_go`, `Copy::retains`), how a group is picked and handed out once (`poll_lowest`, `deliverable`, `take`), or when the track ends (`poll_end`), or when a failover bug needs every ordering of arrivals, switches, and route failures checked rather than a random sweep.
---

# Verify track resume

`rs/moq-net/verify/resume/ResumeReader.tla` models one `Reader` of a logical track that `resume::Producer` serves across route changes. Each route is a received track whose groups land in any order. The front serves one route at a time, switches to another live route at will, and ends the track cleanly once the serving route completes, or unroutable once no route is left. One reader poll (`poll_lowest`, through `sync` and `backfill_gaps`) is one atomic step, as the reader's lock makes it; so is a switch, which the reader follows at once as `serve_from` does. TLC checks every ordering of arrivals, route failures, switches, polls, and finished reads under the bounds in each config.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `OnceEach` | A group is handed out at most once, whichever copies deliver it. | A viewer plays a group twice after a failover. |
| `CleanEndIsComplete` | A clean end comes with every group the serving route held once complete, and every group all live routes hold. | The track ends cleanly with a hole a route could still fill; the viewer loses media silently. |
| `EndWaitsForReplaced` | The serving copy draining ends the track only once no copy it replaced may still deliver a group the reader lacks below that end. | A group still in flight on the old route is lost at the end. |
| `CleanEndIsDeclared` | A clean end only follows a route declaring one. | A failed route reads as the broadcast ending. |
| `FailsOnlyUnroutable` | The track fails only once the front gave up: a route failing is the front's to replace. | A single relay failure ends playback that another route could carry. |
| `QuietIsSettled` | Once the serving route is complete and the reader holds every group, a poll ends the read rather than parking. | A finished track hangs open on a replaced route. |
| `CopiesBounded` | However often routes flap, a route keeps at most its own copy and one backfill of it subscribed. | Each flap leaks a subscription upstream. |

## Run

```sh
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla
quest gate just rs tla moq-net/verify/resume/ResumeReader.tla -config rs/moq-net/verify/resume/ThreeRoutes.cfg
```

The first checks every rule over two routes, three groups, two switches, and a reader holding each group until it finishes it: about 14.3M distinct states, in minutes. The second trades a group for a third route, so a route can come up after another failed: about 7.1M states, in minutes. Both pass at the sync commit. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Open findings

None at the sync commit.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path as a test in `model/resume.rs`'s `mod test`, one call per step: `received_copy` for a route, `routes.serve(copy.consume())` for a switch, `write` for a landing group, `finish()` for a declared end, `abort(..)` for a route failing, and `drain` or `poll_recv_group` for a poll. Pin a path the code follows as a `residual_*` test with its own config, marked with an `\* Open finding:` line, so the others still pass.

## Coverage

Sync commit: `103052ed074a87f00269ecb0d639905482ecfb6f`. Run `git log 103052ed07.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync` (reuse of a route's copy, new copy at `newest`) | `Sync`, `Serve` |
| `rs/moq-net/src/model/resume.rs` `Reader::backfill`, `backfill_gaps` | `Backfill`, `BackfillGaps` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_lowest`, `deliverable`, `take`, `observe` | `Poll`, `Scan`, `Peek`, `Deliverable`, `Take` |
| `rs/moq-net/src/model/resume.rs` `Reader::bound_at_end`, `let_go`, `Copy::retains` | `BoundAtEnd`, `LetGo`, `Retains` |
| `rs/moq-net/src/model/resume.rs` `Reader::poll_end` | `PollEnd` |
| `rs/moq-net/src/model/resume.rs` `Producer::splice`, `serve_from`, the front's end | `Serve`, `FrontFinish`, `FrontFail` |
| `rs/moq-net/src/model/track.rs` `TrackState::is_complete`, `may_land`, `max_key`, `track::Subscriber::has_unread_group` | `Seal`, `Held`, `MayLand`, `Latest`, `HasUnread` |

## Not covered

Whole groups only, so no frames and no `Recover`: a group whose copy stalls or fails mid-read continuing on another copy is left out, as are compressed groups and fetches. The subscription is unbounded from the oldest group with an hour's budget, so nothing goes stale or expires, and local ranges (`set_range`, a lowered floor's backfill), ordered reads, caps, refusals, drops and their reports, and re-offers are left out. Copies answer at once (no `Sub::Pending`), the reader joins before any group lands, a route declares its end only once it holds the last group, and a route that declared its end never fails. A pass covers two routes with three groups or three routes with two, two switches between live routes, and one reader. `resume_model.rs`'s proptest and the tests in `resume.rs` own the rest.

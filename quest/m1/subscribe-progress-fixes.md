# [L] Fix non-wire subscribe progress, wakeup, and cache-invariant bugs

## Goal

Fix every non-wire subscription stall, deadlock, missed wakeup, and cache-invariant violation surfaced by the 2026-11-24 `moq-net` subscribe/fetch audit, without changing any wire format or public API.

## Plan

Created 2026-11-24 (maintainer, /quest-plan of the exhaustive `moq-net` subscribe/fetch audit). Placed in `m1` because these fixes change neither the wire nor public APIs and can land independently of the `m2` [lite-07 subscribe redesign](/quest/m2/lite07-subscribe/README.md). Do not duplicate work already owned by open PRs `#9437`, `#9438`, `#9529`, `#9530`, `#9537`, and `#9546`.

Bugs to fix (each with a deterministic regression test using mocked time):

- **Critical stalls and deadlocks:**
  - **S1** (`rs/moq-net/src/lite/subscriber.rs:11441-11450, 11667-11681`): Open-ended `Start::Time(t), end: None` answered `start: None` fails `reaches` (`range.end == None`), never records `(t, None)` in `serving.time_starts`, and stalls forever once a later group `> t` arrives.
  - **S2** (`rs/moq-net/src/lite/subscriber.rs:11704-11718`): `join_probes` unanswered branch (`join_ticks == None`) checks `now >= join.budget` instead of `wide = asked.min(now) >= join.budget`, stalling a live join retuned wider before `SUBSCRIBE_START`.
  - **S4** (`rs/moq-net/src/model/record.rs:304-419`, `rs/moq-net/src/lite/publisher.rs:6124, 6370-6397`): `Record::retire` and `Record::fold` fold unserved keys moved out of range into `delivered` + `Report::Settled`, permanently stalling a subscriber that later re-adds a range over those keys. Never fabricate `Report::Settled` for unserved keys.
  - **S5** (`rs/moq-net/src/lite/subscriber.rs:14754-14765`): `ServeLoop::poll` returns `Poll::Pending` immediately on `active.stream.writer.poll_flush`, starving response reads, `fetches`, `range_fills`, `unpark`, and idle cancellation, and deadlocking bidirectional Subscribe Streams when both send windows fill. Drive reads and fills concurrently with flushing.
  - **S6** (`rs/moq-net/src/model/track.rs:1589-1606, 10931-10983`, `rs/moq-net/src/model/group.rs:3271-3282`): `Handle::start` short-circuits after frame 0, so writing frame 1+ with `ts > t`, calling `finish()`, or `finish_at(start + 1)` makes `slot.places(t)` true without waking `TrackState` waiters parked in `poll_time_start` / `Subscriber::poll_start`.
- **Ordered cursor, resume, and duplicate stream progress:**
  - **S7** (`rs/moq-net/src/model/track.rs:11741-11871`): `Cursor::poll_next_with` misses `expire_heads` and expiry wake registration on `lacks_head`, allowing a stalled head fill to block an ordered reader past its budget.
  - **S14** (`rs/moq-net/src/lite/subscriber.rs:256, 1468-1513`, `rs/moq-net/src/tail.rs:188-192`): `Open::Duplicate` never calls `Reading::park()`, leaving `tail.active >= 1` and pinning `Settle`/`Recovery` if the primary stream stalls.
  - **S15**: Fix `Cursor::poll_time_start` local-track `Duration::MAX` stall (`model/track.rs:10931-10983`), `Recover::poll` stalls on `Cause::Stalled` / `Cause::Closed` with lingering staged copy / `poll_fetch` `Duration::MAX` (`model/resume.rs:3756-4017`), `Front` stranding on transient `Unroutable` (`model/front.rs:881-925`), and `FinishedServe::probe` skipping `time == u64::MAX` without refusing (`lite/subscriber.rs:13025`).
  - **D6** (`rs/moq-net/src/model/track.rs:12596-12647`): `Subscriber::ordered` over `Inner::Resume` must set `key_order = true` on copy cursors (coordinate `take_withdrawn` with `#9437`).
  - **D7** (`rs/moq-net/src/model/resume.rs:2970-2988, 3479-3596`): `Reader::backfill_gaps` must re-arm when a draining replaced copy loses groups to eviction or expiry.
  - **D8** (`rs/moq-net/src/lite/subscriber/fills.rs:529-554`, `rs/moq-net/src/lite/subscriber.rs:12255-12258`): `RangeFills` draining loop ignores `read`, keeping upstream `SUBSCRIBE` open and blocking `IDLE_LINGER` after all downstream readers leave.
  - **D9**: Stop reverting `TrackState::start` to `FeedStart::Pending` for 1 RTT on every new demand range (`lite/subscriber.rs:11383-11397`); prevent `fit_after` from overflowing `MAX_MESSAGE_SIZE` on initial `SUBSCRIBE` or starving live joins behind time-start probes (`lite/subscriber/fit.rs:46, 66`); fix lost-wakeup race in pre-lite-05 `FeedFetches::poll` (`lite/subscriber.rs:14016-14026`); merge overlapping `Oldest..=a` and `Oldest..=b` in `model/track/demand/ranges.rs`.
- **`AGENTS.md` cache-invariant fixes (`D10`):**
  - `rs/moq-net/src/model/track.rs:1240-1254, 1331-1435, 2114-2225, 2513-2916`: Fix `containing` on `Local`/`Refused` resolving against arbitrary cached groups, `join` falling back to cache after declared end, `forget_drops` erasing proven answers, `successor()` jumping over evicted groups (making unread groups appear fresher), and budget-widening re-offer depending on cache retention (`prune_skipped` vs `Misses`).

Verify any `model/resume.rs` changes with `/verify-resume` (`ResumeModel.tla`).

Public API: none. Wire: none.

## Related

- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - wire and public API half of the same 2026-11-24 subscribe/fetch audit

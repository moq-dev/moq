# [S] A resumed group ends once the new copy is past it

## Goal

A reader that awaits only the in-flight group's `read_frame` never parks forever
after its source fails mid-group. Once the track fails over to a new copy, the
half-delivered group either completes from that copy or ends with an error, on
every lite version and for local and remote sources alike.

Two reports, one mechanism:

- #4392: a publisher aborts a track mid-group over a session. On lite-03/04 the
  far subscriber's in-flight `read_frame` stays pending forever; on lite-05/06
  it errors.
- #4365: a publisher aborts a track mid-group and re-creates it under the same
  name in one origin. The origin reader parks in `read_frame` while a direct
  reader gets the abort.

## Plan

Why it hangs: `track_ended` in `rs/moq-net/src/model/front.rs` treats a copy
that dies after delivering as failover and splices a new copy. The resumed
`resume::Group` then waits in `track.poll_peek_group`
(`rs/moq-net/src/model/resume.rs`) on that copy, but never asks it for
anything. Only `resume::Subscriber::apply`, driven by `recv_group`, subscribes
to a new copy. On lite-03/04 the re-query accepts the track without asking the
peer (no TRACK stream), so no SUBSCRIBE goes out and the refusal never comes
back. An in-process copy declares no start, so its peek answers "not yet"
forever. The doc comment on `resume::Group` promises that a group reader alone
picks up the continuation; that promise is what breaks.

Decided:

- While a resumed group waits on a copy, it holds a subscription on that copy
  with `start = latest`, the same shape as a fresh subscriber. A refusal ends
  the group with that error.
- If the copy's latest group is past the resumed group, the group ends with an
  error at once. A same-name re-create is a continuation, so group 2 of the old
  track ends and group 3 of the new one arrives. No special case for local
  broadcasts.
- If the resumed group is the copy's latest, it reads the rest from that
  subscription. A copy that later drops it says so with SUBSCRIBE_DROP once
  [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) lands; until then that case
  waits as today.
- No FETCH, not even where the version supports it. Fetching older groups from
  the new copy may come later as its own quest.
- Don't abort the old group eagerly on failover instead: long-lived groups
  like a catalog must survive a failover.

`poll_finished` needs the same subscription as `poll_current`. Drop the
subscription once the wait resolves.

A prototype of the subscription half (about 20 lines in `resume.rs`) turned
all eight #4392 cases into errors with every moq-net test passing.

Regression tests, on a paused clock (`#[tokio::test(start_paused = true)]`):

- The #4392 repro as `rs/moq-net/tests/track_abort_over_session.rs`: abort
  mid-group over one and two hops on lite-03 through lite-06; the in-flight
  read errors.
- The #4365 repro: abort mid-group 2, re-create at group 3 through the origin;
  group 2 ends with an error and group 3 arrives.

## Closes

- [#4392](https://github.com/moq-dev/moq/issues/4392) - close this issue when the quest finishes
- [#4365](https://github.com/moq-dev/moq/issues/4365) - close this issue when the quest finishes

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - tells a resumed latest group when the new copy dropped it
- [Splice edge cases](/quest/m1/splice-edges.md) - the same splice code and test harness

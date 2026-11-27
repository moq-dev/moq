# [M] Stream credit's lite-07 fetches come back whole under load

## Goal

`just test stream-credit --version moq-lite-07-wip` passes its fetch check (`every group the tab
fetched came back whole`) on both engines on a loaded host, fixed at its two causes, never by a
looser assertion.

## Plan

Found on 2026-11-27 by
[moq-dev/moq#9743](https://github.com/moq-dev/moq/pull/9743), once Firefox's subscriptions stopped
hanging. On a host at load 25 to 37 (32 cores), 4 of 8 Firefox lite-07 runs and the one Chromium
lite-07 run on `main` failed it, with 2 to 57 groups each. A scheduled Interop runner has not hit
it: Chromium lite-07, which takes the same drop path, passed every scheduled run from 2026-11-21
to 2026-11-26. [moq-dev/moq#9740](https://github.com/moq-dev/moq/pull/9740) saw the Chromium half
too. Every failing group read `0 frames, then` one of two errors, each with its own cause, both
confirmed with temporary debug logs in the relay's `lite::publisher`:

- `TooFarBehind` (most groups): the relay preempted the fetch's own Group Stream for credit. On
  lite-07 a fetch is a one-shot bounded SUBSCRIBE, so its group rides a stream the relay opens,
  under the same browser cap as the live groups. The tab fetches at priority 0, below the mid tier
  it fetches for. The draft says a subscriber that wants the rest of a preempted group asks again,
  so recommended: the tab fetches again while a fetch stops with `TooFarBehind`, keeping the
  check that the group comes back whole. That is the protocol's own recovery for a preempted
  group, not a test retry, so the epic's no-retry rule does not apply. Rejected: fetching above
  every tier, which takes credit from the high tier, whose every group must arrive.
- `Old` (52): the relay served the one-shot SUBSCRIBE a group whose copy had failed with `OLD`.
  The copy's upstream subscription is a live join with a max of 0, so the publisher reset the
  copy's stream as soon as the next group started. That judged the copy's delivery, not whether
  the group exists, and the one-shot reader has no live join at all, so upstream would answer it
  with the group. The relay served the request as a normal subscription (its logs showed
  `serving group` on the request's id, so the subscriber's FIN came after serving began), not
  through `OneShotFetch`, whose routed cursor already excuses `Old`. `GroupServe::poll_open` gives
  it up through `poll_given_up` (`group.poll_finished` returns the copy's `Old`), since
  `Recover::undelivered` in `rs/moq-net/src/model/resume.rs` excuses `Old` only for a fetch (`reader` is `None`), not for a
  subscription reader whose own budget never reaches the group. Recommended: treat a copy's `Old`
  as undelivered for a reader whose budget is wider than the max the copy's live join was judged
  under, as the relay already does for an `OLD` drop, so the group recovers from upstream. Model it
  with the verify-resume skill first, since it changes which errors end a group, and mirror it in
  `js/net/src/resume.ts`.

Reproduce with `just test stream-credit --version moq-lite-07-wip` on a loaded host, or under
`stress`, and count failing groups per run before and after.

Public API: none. Wire: none.

## Related

- [A reader that still wants a group reset to make room fetches it](/quest/m3/fetch-preempted-group.md) - moves the tab's fetch into the ordered reader, which hits the same preempted one-shot SUBSCRIBE

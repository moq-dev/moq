# [S] js/net leaks nothing on a long listing or an early subscription end

## Goal

`@moq/net` holds no listener, resolver, or group past its use: a front's
track listing does not grow per TRACKS event, a lite publisher closes a held
first group when its subscription ends first, and the ietf publisher sends
END_OF_TRACK only when the subscription's range reaches the track's end, as
Rust does.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first.

- `js/net/src/resume.ts` `Front.#list`: `race([listing.next, wait()...])`
  leaves the inner race's three registrations pending each time
  `listing.next` wins. The same pattern is in `lite/subscriber.ts`
  (`reoffers.changed()`). Race the change waits directly. From #6158 and
  #8476.
- `js/net/src/lite/publisher.ts`: `first = { group, largest }` is held while
  the source start is pending, and the early exits and the `finally` never
  close it, so a relayed filled group keeps reading upstream for nobody. Close
  it in the `finally`. From #6206.
- `js/net/src/ietf/publisher.ts`: `#runEndOfTrack` is sent unclipped to a
  bounded subscription's range; port Rust's `reached` check from
  `TrackServe::end` ([moq-dev/moq#5283](https://github.com/moq-dev/moq/pull/5283)).
  On `release` too.

Public API: none. Wire: none.

# [M] A JS relay judges a relayed FETCH by its budget

## Goal

A JS publisher serving a lite FETCH for a broadcast it consumes over the wire
(through a `Front`) resets the response with OLD once the group goes stale
while the reply waits on flow control or its acknowledgement, as it already
does for a broadcast it produces and as a Rust relay does.

## Plan

Found reviewing [moq-dev/moq#9140](https://github.com/moq-dev/moq/pull/9140).
`js/net/src/lite/publisher.ts` `#serveFetch` judges the group with `GivenUp`,
but a group from `Front.fetchGroup` comes from the wire
(`lite/subscriber.ts` `fetchGroup`) with no expiry attached, so only an
upstream reset or failure gives it up. Once the upstream reply is
acknowledged, nothing judges the downstream one. Rust has no gap: a relayed
fetch fills the track cache, whose policy judges it.

Recommendation: attach an expiry judged by the route copy that holds the
track, as the routed draft-22 fill does through `ResumeSource.staleAt`,
without opening a subscription: a fetch-only relay must not start pulling the
track upstream. With no copy subscribed, nothing newer is known locally, which
matches Rust's cache.

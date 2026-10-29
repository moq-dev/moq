# [M] A quick finish keeps its pre-draft-22 join

## Goal

On draft 18, a joining FETCH that reaches our publisher after its
subscription finished cleanly is still answered from the subscription's
saved Largest group. A track that ends within about one RTT of SUBSCRIBE_OK
then finishes cleanly at the subscriber, head included, instead of losing
its last group.
What the publisher retains for this is bounded by design (the session, the
subscription's request, or a limit the peer already negotiates), never by a
timer constant.

## Plan

Every pre-draft-22 join is a SUBSCRIBE followed, after SUBSCRIBE_OK, by a
`RelativeJoining{0}` FETCH ([moq-dev/moq#6348](https://github.com/moq-dev/moq/pull/6348)).
A refused one costs the subscriber that group's head and nothing more
([moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407), decided with
the maintainer on 2026-10-22): a stream starting mid-group is dropped and
the subscription goes on.

State: `rs/moq-net/src/ietf/publisher.rs` registers a `Join` entry in
`Publisher::joins` when it dispatches the SUBSCRIBE, fills it with a `Joined`
snapshot as SUBSCRIBE_OK goes out, and drops it when the subscription ends,
before PUBLISH_DONE. A joining FETCH that arrives later finds no entry, waits
out the registration wait in `joined()`, and is refused as an unknown
subscription, so the subscriber loses the Largest group. Short or
already-finished tracks hit this often.

It flakes the pinned-pass interop `tail` cells with a Rust publisher, about
one run in six (seen 2026-10-22 while landing
[moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407)): the tail test
finishes its track before SUBSCRIBE_OK, so PUBLISH_DONE follows within
milliseconds. The relay's settle grace (about a second) ends long before the
10 s registration wait refuses the FETCH, and the empty subscription stream
for the Largest group then becomes an empty group, which `rust -> rust` and
`rust -> js-native-*` read as "empty group".

Fix it at the publisher: the subscriber cannot recover a head nobody serves,
and other publishers may refuse the same way.

A finished entry holds a `track::Consumer`, which keeps the track's cache
alive, so retaining it costs memory a peer controls. Candidate bounds:

- **The request.** Draft 18 gives each SUBSCRIBE its own request stream;
  keep the entry until the subscriber closes its side, with our subscriber
  holding it open until its joining FETCH resolves. A peer that never sends
  the FETCH costs no more than an open subscription, which stream credit
  already bounds.
- **The session alone.** Grows with every subscription a long-lived relay
  session ever makes, so probably not bounded enough.

Recommendation: the request close. Keep only entries that can be answered (`Joined::Group`); an
unsubscribed or refused subscription still drops its entry at once. The
entry must not outlive what authorized the subscription: when its request
token's grant expires or is revoked, or the session grant no longer covers
the track, drop it, since a late FETCH reads the cache on that authority. A head
evicted from the cache meanwhile stays a loud refusal. The existing wait in
`joined()` bounds a FETCH whose subscription has not arrived yet; do not
stretch it to cover this.

Tests, on mocked time with no sleeps, on draft 18:

- A track that finishes right after SUBSCRIBE_OK, with the joining FETCH
  delivered an RTT after PUBLISH_DONE: the FETCH returns the Largest group's
  head and the subscriber's track finishes cleanly with it. Today the FETCH
  is refused and the group is lost.
- Closing the subscription's request drops its entry.
- A request token that expires (by mocked time) or is revoked after the
  finish drops the entry, and the late FETCH is refused.
- An unsubscribed subscription's FETCH is refused, and session close frees
  every entry.

Public API: none. Wire: none; a late joining FETCH is answered where it was
refused.

## Related

- [JS joining FETCH](/quest/m2/ietf-js-joining-fetch.md) - the JS publisher mirrors the bound settled here

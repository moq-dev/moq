# [S] An abandoned FETCH is released along the mesh

## Goal

When nobody wants a FETCH's group any more, the lite subscriber in Rust and in
`@moq/net` closes its upstream FETCH stream, so a relay stops pinning the
request and the publisher behind it stops serving. With
[#4531](https://github.com/moq-dev/moq/pull/4531)'s publisher side, an
abandoned FETCH is then released across every hop.

## Plan

#4531 made the lite publisher stop serving a FETCH once the requester FINs or
resets, but the Rust subscriber's `FetchServeRun` never watches demand, so a
relay holds its upstream FETCH until it is answered. Watch demand for the
whole wait and close the stream when it leaves, the same way the subscriber
already drops an unwanted SUBSCRIBE. Check `@moq/net`'s subscriber for the same
gap; #4531 found it FINs an abandoned FETCH, so confirm it does so on demand
loss and not only on close.

Decided with the maintainer: both subscribers, proven end to end. A relay-chain
test (publisher, relay, subscriber) abandons a FETCH mid-wait and asserts the
upstream publisher's serve ends, which #4531 could only test in-crate.

Public API: none. Wire: none.

## Related

- [Cross-relay bursts](/quest/m1/cross-relay-bursts.md) - unanswered FETCHes across relays

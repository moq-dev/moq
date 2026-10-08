# Pipelined requests

## Goal

A track request never waits a round trip for the track's info before the
request that carries data goes out: SUBSCRIBE and the first FETCH travel with
TRACK (lite) or TRACK_STATUS (moq-transport) at every hop, so first data
arrives one round trip sooner per hop. Peers that send them serially keep
working unchanged.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview: the extra round trip is not
worth keeping, for SUBSCRIBE and FETCH alike. Shared rules for both children:

- Data that arrives before the info stays unread in QUIC until the info lands.
- A failed or reset info request fails the data request.
- Legacy serial peers keep working, proven by mixed-mode tests in Rust and JS.

End to end: measure time to first frame and first fetched group across one
and two relay hops, before and after the line.

## Required

- [SUBSCRIBE goes out with TRACK](/quest/m1/pipeline-requests/subscribe.md) - subscribers open TRACK and SUBSCRIBE together in Rust and JS
- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - a fetch-only reader's first FETCH goes out with the info request, on lite and moq-transport

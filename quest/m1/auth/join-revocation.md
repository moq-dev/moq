# [S] A revocation never waits behind a joining FETCH

## Goal

On the drafts that still send a joining FETCH (before draft 20), our own
subscription ends as soon as its grant is revoked, narrowed, or expires,
even while its joining FETCH waits for request-ID credit or a stream.

## Plan

A #5148 follow-up. At #5148's head (`ed595beef`), after SUBSCRIBE_OK the
subscriber sends its joining FETCH through `start_joining_fetch`
(`rs/moq-net/src/ietf/subscriber.rs`). That function awaits
`next_request_id`, which gives up after ten seconds without credit, and then
a stream open. Only then does the subscription loop start polling its gate
and terminals. A revocation in that window waits for the credit wait to end.
#5148 fixed the same shape for token renewal: it polls the wait alongside the
subscription's terminals, so a peer that withholds credit cannot hold a
revocation back.

Root problem: a step of the subscription's setup runs outside the loop that
races the grant. Bring the joining FETCH's credit wait and open under the same
terminals. Keep the joining FETCH's existing fallback to live, and keep
request IDs going out in allocation order on drafts 14-16.

Regression on mocked time, draft 14, no request-ID credit: revoke the grant
while the joining FETCH waits. The subscription ends `Unauthorized` at once,
not after the ten-second give-up. Check every draft's setup path for any
other await that is not under the gate.

Public API: none. Wire: none.

## Required

- [Request tokens](/quest/m1/auth/request-token.md) - #5148 adds the grant-aware subscription loop this extends

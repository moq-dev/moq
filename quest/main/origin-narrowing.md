# [M] A live origin grant narrows in place

## Goal

A `moq-net` origin handle's grant can be replaced by a narrower pattern union
while the session is live, shared by every handle derived from it: announce
cursors retract the prefixes that fell outside, and requests and publishes
outside it are refused. The relay narrows a session in place when a
re-validation narrows its grant and nothing handed out falls outside the new
one; otherwise it closes the session as before.

Ending what was already handed out (the deafen case #2714 asked for) is
[Handout revocation](/quest/next/handout-revocation.md).

## Plan

- `origin::Producer::narrow` and `origin::Consumer::narrow` set a ceiling on
  the handle's grant. Each `scope` call derives a grant node, so the ceiling
  reaches every handle derived below it and nothing above. A grant that is not
  a subset of the current one is refused with `Unauthorized`.
- A narrowing that would remove a live served broadcast, or a route the grant
  published, is refused with `Unsupported` and changes nothing. Served
  broadcasts carry no owner, so any live one in the removed part counts.
- Ceilings live in the origin state under the lock every check already takes,
  so an origin that never narrows pays one emptiness check per request and
  nothing on the announce fan-out.
- `moq_relay::auth::Lease::with_origins` attaches a session's handles; a
  narrower re-check narrows them in place, a widened side keeps what it was
  admitted with, and any refusal closes the session as before.

Public API: additive on moq-net (`narrow` on both handles) and moq-relay
(`Lease::with_origins`). Wire: none.

## Related

- [Handout revocation](/quest/next/handout-revocation.md) - ends what a narrowing removes instead of closing the session

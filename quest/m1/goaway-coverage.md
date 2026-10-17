# [M] GOAWAY reaches every IETF request stream

## Goal

On moq-transport, every request stream in Rust and JS (publisher and
subscriber side, including FETCH, TRACK_STATUS and SUBSCRIBE_TRACKS) handles
a GOAWAY the way SUBSCRIBE does, a control stream that ends or resets is a
PROTOCOL_VIOLATION, and draft-18's Request ID cutoff answers requests past it
with GOING_AWAY.

## Plan

Found by #5800 (2026-10-17 state of main):

- Only SUBSCRIBE and SUBSCRIBE_NAMESPACE on the publisher side read GOAWAY
  on their request streams (Rust `request_stream`, JS `nextMessage` and
  `namespaceUpdate`). FETCH, TRACK_STATUS, SUBSCRIBE_TRACKS, and every
  subscriber-side request stream end the request on a GOAWAY instead.
- A control stream FIN is treated as a clean end: Rust's draft-17+ GOAWAY
  reader returns Ok, the draft 14-16 adapter tears the session down with no
  code, and JS closes with no code. Draft 21 section 6.3 makes it a
  PROTOCOL_VIOLATION; check the SETUP STOP_SENDING ordering too.
- Draft 18's GOAWAY Request ID is only parity-checked. Requests at or past
  the cutoff never get GOING_AWAY, and our own GOAWAY sends a fixed 0 or 1.
- Check JS for a duplicate-GOAWAY race.

Decided 2026-10-17 (maintainer): m1, beside drain cleanup. Draft 17 stays
low priority; fix it where the shared code makes it free.

Public API: none expected. Wire: closer conformance on drafts 14-22; no new
fields.

## Related

- [Drain cleanup](/quest/m1/drain-cleanup.md) - what a drain does to sessions at its deadline
- #5798 - the legacy drafts' request handling

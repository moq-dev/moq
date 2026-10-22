# [M] Interop covers GOAWAY, TRACK_STATUS, datagrams, and request limits

## Goal

On the IETF matrix (`just test interop --versions ietf`), across
Rust and JS: a GOAWAY with a new URI moves the session and its
subscriptions resume there; TRACK_STATUS gets a real answer from the other
implementation; datagram objects cross moq-relay intact; and a subscriber
opening more concurrent tracks than its MAX_REQUEST_ID grant waits for
credit instead of failing (drafts 14 and 16; drafts 17 and later have no
MAX_REQUEST_ID).

## Plan

Decided with the maintainer on 2026-10-21: in-tree cases, not proposed to
the runner's spec. `test/smoke/cloudflare.sh` still says moq-relay does no
datagrams; fix that comment, and add a datagram round to its moq-dev
section if it is cheaper than a matrix case.

Public API: none. Wire: none.

## Progress

The implementation PR adds relay datagrams in both Rust/native-JS directions on
all five drafts, JS wire TRACK_STATUS queries to Rust on all five, and Rust
query-before-subscribe to JS from draft 18. Draft-14 real answers exposed missing
TRACK_STATUS_OK/ERROR routing in the JS control adapter; the patch includes
success/refusal/success coverage. A JS peer advertising one usable track slot checks
Rust waiting for credit on drafts 14 and 16. Real Rust-to-JS GOAWAY with a new URI
retains a reader on drafts 18, 20, and 22, with the same publisher epoch and a
replacement-only data barrier before cutting the original transport.

The initial datagram burst and GOAWAY readiness failures were fixture errors.
No production handover policy change is justified by those results.

## Remaining

- Decide what the GOAWAY requirement means on drafts 14 and 16. They cannot carry
  publisher epochs, and the documented model restarts across an unproven route
  rather than splicing a retained reader. Recommend testing application
  re-request/re-subscribe there and keeping same-reader continuity on 18+.
- Add the reverse GOAWAY direction (JS server to Rust reconnecting client) and
  request credit with JS as the subscriber. Keep small-window setup in test peers,
  rather than adding a public configuration knob solely for coverage.
- Retain this quest until the remaining coverage and legacy acceptance are settled.

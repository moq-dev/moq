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

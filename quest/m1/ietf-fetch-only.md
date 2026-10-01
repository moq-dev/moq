# [M] Fetch without SUBSCRIBE

## Goal

A relay serves a FETCH-only demand for an IETF upstream track without
SUBSCRIBE upstream. A finished upstream track can still be fetched, and a
FETCH_OK that reaches the end of the track says so every time.

## Plan

The origin splices a route only once the track's info is known. moq-lite gets
it from TRACK_INFO, so its fetch-only demand never subscribes. IETF has no
TRACK_INFO, so today fetch-only demand still makes the relay SUBSCRIBE upstream.
Two symptoms follow:

- A finished upstream track refuses the SUBSCRIBE, so its groups cannot be
  fetched either.
- The live subscription races the group FETCHes. End of Track is known only
  once an upstream FETCH_OK reports it, so a downstream FETCH that runs to the
  end can answer before that and leave End of Track unset. moxygen's "FETCH
  with large objects" case flakes on this.

TRACK_STATUS is the likely source of the info. The publisher refuses it today,
so both sides are in scope. Keep the SUBSCRIBE path for real subscription
demand.

Decided (2026-10-01):

- Our subscriber learns a track's properties from TRACK_STATUS, sent in
  parallel with every FETCH or SUBSCRIBE. From draft 20 it sets
  INCLUDE_PROPERTIES to 0 on those requests, so the properties aren't sent
  twice. Before draft 20 there's no opt-out, so it ignores the duplicate.
- The publisher serves TRACK_STATUS. Its reply carries what it would set in a
  SUBSCRIBE_OK (d22 §9.13), from the same code that fills SUBSCRIBE_OK and
  FETCH_OK.
- The publisher still follows the spec for subscribers that don't opt out:
  [FETCH_OK properties](/quest/m1/fetch-ok-properties.md).

## Related

- [FETCH_OK properties](/quest/m1/fetch-ok-properties.md) - the publisher side of the same properties

- [JavaScript FETCH](/quest/m1/js-fetch.md) - the browser publisher answers these fetches

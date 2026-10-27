# [L] Serve cached IETF FETCH in JavaScript

## Goal

A JavaScript publisher answers IETF FETCH from a native subscriber for groups
in its cache, standalone and joining, and honors the draft-18 Largest Object.

## Plan

Decided 2026-10-27 (maintainer): split in two. This half, cached and joining
FETCH with the draft-18 Largest fix, starts now: the JS IETF publisher refuses
every FETCH today (`#runFetch` in `js/net/src/ietf/publisher.ts`), while the
JS lite publisher already serves cached FETCH. Serving evicted or uncached
groups through the on-demand request surface `@moq/net` gains in
[JS ranges](/quest/m1/subscribe-ranges/js.md) moved to
[on-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md).

Implement IETF FETCH dispatch and codecs across the supported draft versions.
Cover standalone and relative joining requests, subscription lifetime
bookkeeping, draft-specific FETCH_OK encoding, legal End Location, refusal
codes, cancellation, and clean stream finish. Match the existing Rust response
contract, including saved object prefixes. Unsupported versions or request
forms must receive the protocol's explicit refusal rather than hang.

Datagrams are never fetchable (#4982): Rust answers an IETF FETCH that
reaches a datagram with DOES_NOT_EXIST; match it. Decided 2026-10-08: JS adds
no `NotFetchable` code, since nothing would send it. Only lite-07 has the
code, and lite-07 drops FETCH for [ranges](/quest/m1/subscribe-ranges/lite07-fetch.md);
lite-05 and lite-06 answer `NotFound`.

The JS publisher already answers the TRACK_STATUS a native fetch-only request
asks before its first FETCH, so after that answer the FETCH is all that is
refused today.

Decided 2026-10-25 (maintainer): the joining FETCH lands here, folded from
the m2 JS joining FETCH quest, whose remaining part (answering a FETCH after
the subscription ends) stays in
[JS join after a quick finish](/quest/m2/ietf-js-joining-fetch.md). Today
`js/net/src/ietf/publisher.ts` refuses every FETCH, ignores the filter before
draft 22, and serves the latest group whole from object 0, rounding
SUBSCRIBE_OK's Largest to `{G, 0}`. The two parts below only work together;
[moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407) made a refused
joining FETCH cost the Rust subscriber only that group's head.

1. **Honor Largest Object on draft 18.** `subscribeRange` serves the
   `nextObject` filter from `edge.next`, as draft 22 already does, and
   SUBSCRIBE_OK reports the true Largest on every draft. Otherwise the
   FETCH's head duplicates the subscription's first objects, and the Rust
   subscriber's "a whole group arrived for one the fill already headed" path
   truncates the group to the head. JS subscribers send `unfiltered` before
   draft 22 and are unaffected. A third-party Largest Object subscriber that
   sends no joining FETCH then loses the head, as the draft says it should.
2. **Serve the head.** Keep a per-session registry of joinable SUBSCRIBEs:
   their track, the advertised Largest, the SUBSCRIBE_OK properties and
   timescale, the ranking, and the unsubscribe signal. `#runFetch` resolves a
   joining FETCH against it: unknown subscription is
   `invalid_joining_request_id`, no Largest or a start past it is
   `invalid_range`, more than one group is `not_supported` (as in Rust and
   the draft-22 fill), and a head evicted from the cache is refused before
   FETCH_OK. Then it writes FETCH_OK (end `{G, k + 1}`, not end of track) and
   a fetch stream named by the FETCH's own request ID, reusing
   `#writeFillGroup`. It is not one of the subscription's counted streams.
   Codec work: `Fetch` keeps the joining fields, and a `FetchOk` encoder for
   draft 18 writes empty parameters and Track Properties.

Joining tests: unit tests per draft for each refusal and for a served head
(bytes the Rust subscriber stitches), and the tail cells
`just test interop --tail --publishers js-native-node,js-native-bun --subscribers rust --versions ietf`
plus the browser publisher on draft 18, except a track that finishes
right after SUBSCRIBE_OK, which the m2 quest covers.

Run the supported-draft matrix and `just test interop --all` through CI.

Public API: none. Wire: implement the existing
supported IETF FETCH formats, including the joining FETCH, and serve a
Largest Object subscription from past Largest; update relevant documentation
and any MoQ draft claims that change.

## Related

- [On-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md) - serves evicted and uncached groups once JS ranges lands
- [JS FETCH fill timeout](/quest/m1/ietf-fill-timeout-js.md) - JS timeout budgets and gap dispositions build on this responder
- [JS join after a quick finish](/quest/m2/ietf-js-joining-fetch.md) - answers a joining FETCH that arrives after its subscription ended

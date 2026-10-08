# [M] Serve IETF FETCH in JavaScript

## Goal

A JavaScript publisher answers IETF FETCH from a native subscriber, including
groups that are no longer in its live cache, through the on-demand request
surface `@moq/net` gains in [JS ranges](/quest/m1/subscribe-ranges/js.md).

## Plan

Decided in the 2026-10-05 audit: the producer-side request surface folded
into [JS ranges](/quest/m1/subscribe-ranges/js.md), so it takes range
requests from the start. This quest keeps only IETF FETCH dispatch onto it.

Implement IETF FETCH dispatch and codecs across the supported draft versions.
Cover standalone and relative joining requests, subscription lifetime
bookkeeping, draft-specific FETCH_OK encoding, legal End Location, refusal
codes, cancellation, and clean stream finish. Match the existing Rust response
contract, including saved object prefixes. Unsupported versions or request
forms must receive the protocol's explicit refusal rather than hang.

Datagrams are never fetchable (#4982): Rust answers an IETF FETCH that
reaches a datagram with DOES_NOT_EXIST; match it. Decided 2026-10-08: JS adds
no `NotFetchable` code, since nothing would send it. Only lite-07 has the
code, and lite-07 drops FETCH for [ranges](/quest/m1/subscribe-ranges/lite.md);
lite-05 and lite-06 answer `NotFound`.

Verify with an in-memory application responder: a browser publisher serves a
native IETF subscriber after a group is evicted or was never cached. Run the
supported-draft matrix and `just test interop --all` through CI.

Public API: none beyond JS ranges' surface. Wire: implement the existing
supported IETF FETCH formats; update relevant documentation and any MoQ draft
claims that change.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the on-demand request surface this dispatches onto

## Related

- [Browser archive](/quest/m3/archive-browser.md) - supplies memory or OPFS archive data through the same request surface

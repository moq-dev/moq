# [L] Serve on-demand groups and IETF FETCH in JavaScript

## Goal

A JavaScript publisher serves requested groups that are no longer in its live
cache, including IETF FETCH from a native subscriber. The capability is
independent of archives and works for arbitrary track payloads.

## Plan

Add the producer-side counterpart of `track::Consumer.fetchGroup`, matching
Rust's `track::Dynamic` request ownership and accept/refuse lifecycle. Expose
an owned request handle rather than a storage callback; dropping a request
must refuse it rather than leave the subscriber waiting. Preserve group
sequence, frame boundaries, payload bytes, and track properties. Storage,
retention, and media catalog interpretation remain outside `js/net`.

Use one logical dynamic track per broadcast/name. Keep cache-miss group
requests distinct from creation of a new live track producer, and cover
concurrent requests and cancellation without creating duplicate live producers.

Implement IETF FETCH dispatch and codecs across the supported draft versions.
Cover standalone and relative joining requests, subscription lifetime
bookkeeping, draft-specific FETCH_OK encoding, legal End Location, refusal
codes, cancellation, and clean stream finish. Match the existing Rust response
contract, including saved object prefixes. Unsupported versions or request
forms must receive the protocol's explicit refusal rather than hang.

Use an in-memory application responder for verification, without OPFS or an
archive writer. A browser publisher serves a native subscriber after a group
is evicted or was never cached; verify exact group/frame replay, empty groups,
missing groups, concurrent requests, and cancellation while awaiting a reply.
Run the supported-draft matrix and `just test smoke-full` through CI.

Public API: a producer-side on-demand group request surface in `@moq/net`,
matching Rust's lifecycle. Wire: implement the existing supported IETF FETCH
formats; update relevant documentation and any MoQ draft claims that change.
Target published API breaks at dev if the chosen shape requires them.

## Required

- [Dynamic track identity](/quest/next/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - settles shared producer identity before adding on-demand group requests

## Related

- [Browser archive](/quest/next/archive/browser.md) - supplies memory or OPFS archive data through this generic request surface

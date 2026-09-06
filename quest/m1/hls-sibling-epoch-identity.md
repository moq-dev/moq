# [M] Bind sibling media to the epoch described by its catalog

## Goal

An HLS export must not serve a replacement sibling publisher's restarted groups
under a catalog and timeline describing the previous publisher, including when
the replacement happens before the sibling request resolves.

## Plan

`moq_mux::Source::bind` starts one request at rendition construction and retains
its result. This closes the first-segment lookup window, but does not establish
an epoch relationship between two separate broadcasts. A dynamic handler can
answer an already queued request with a replacement broadcast; a remote route
also resolves asynchronously. A replacement already present at construction is
indistinguishable from the sibling intended by the catalog.

Define how a catalog reference identifies the intended media epoch, and how an
export validates that identity before serving groups. Merely awaiting the
request before advertising timeline rows does not validate existing timeline
records against the publisher that answers. Coordinate catalog format changes
with the matching hang draft and JS implementation if an identifier is needed.

Add a deterministic regression with a held catalog and timeline, a pending
sibling request, and a handler that answers with a replacement after timeline
rows arrive. Also cover a replacement already present when the export starts.

## Related

- [Closed publisher status](/quest/m1/hls-closed-publisher-500.md) - classify unavailable media once publisher identity is known

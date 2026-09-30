# [S] Draft-20 FETCH

## Goal

On drafts 20 and later, a FETCH whose LOCATION_FILTER names whole groups is
answered the way drafts 14 to 19 answer a standalone FETCH: from cache, with
each miss fetched upstream one group at a time, and an upstream refusal passed
through.

## Plan

[Legal IETF input](/quest/m0/ietf-legal-input.md) decodes the draft-20 layout
and refuses it `NOT_SUPPORTED`. Replace that refusal with the existing
standalone walk over the filter's range, on the publisher and on the relay's
upstream group fill. A filter the walk cannot express stays an explicit
refusal.

Update the draft-20 note in `doc/concept/standard.md`.

## Required

- [Legal IETF input](/quest/m0/ietf-legal-input.md) - decodes the draft-20 FETCH this serves

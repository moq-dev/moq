# [S] Draft-20 FETCH

## Goal

On drafts 20 and later, a FETCH whose LOCATION_FILTER stays within one group
is answered the way drafts 14 to 19 answer a standalone FETCH: from cache, with
a miss fetched upstream, and an upstream refusal passed through. A range
touching several groups is refused `NOT_SUPPORTED`, as on older drafts.

## Plan

[Legal IETF input](/quest/m0/ietf-legal-input.md) decodes the draft-20 layout
and refuses it `NOT_SUPPORTED`. Replace that refusal with the existing
single-group read, on the publisher and on the relay's upstream group fill.

Update the draft-20 note in `doc/concept/standard.md`.

## Required

- [Legal IETF input](/quest/m0/ietf-legal-input.md) - decodes the draft-20 FETCH this serves

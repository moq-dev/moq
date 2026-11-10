# [S] moq-transport stops sending our max age as MAX_CACHE_DURATION

## Goal

A late joiner reads the newest catalog, and the newest group of every other
track, through any draft-18 relay. Neither the Rust nor the JS moq-transport
encoder writes `MAX_CACHE_DURATION` for a track's max age. A received
`MAX_CACHE_DURATION` still decodes into `max_age`.

## Plan

Decided 2026-11-10 (maintainer, `/quest-import` of
[moq-dev/moq#8465](https://github.com/moq-dev/moq/issues/8465)):
✅ never send the property (rejected: omit it only when zero; a subscriber
joining FETCH whenever a duration is declared).

Why: our max age expires only superseded groups, while draft 18 §12.3
expires every object, the newest included, the given time after it arrives.
moq-transport has no way to say "the newest group never expires", so any
value, not only the zero that
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) made the
default, makes a conformant relay drop a long-lived group. moqx drops the
catalog at once, so a late joiner exports 0 bytes. Omitting the property
fixes third-party subscribers too, unlike a FETCH on our side, and adds no
IETF code after the [scope freeze](/quest/m2/ietf-scope/README.md).

Cost, accepted: across a moq-transport hop between our own relays the max
age is lost, so the downstream relay keeps superseded groups as long as its
cache pool allows.

The encode sites are `Properties::restated` and `Properties::encode` in
`rs/moq-net/src/ietf/properties.rs` and `maxCacheDuration` in
`js/net/src/ietf/publisher.ts`. Keep a property the publisher sealed inside
Immutable Properties forwarded as it was sent. Test it the way the issue
reproduced it: a subscriber that joins after the catalog's only group was
written still reads it through a relay that honors §12.3, if the interop
runner can run one, and otherwise with a unit test on what gets encoded.

m0 because #7729 is on `main` but not released, so the next cut would ship
the regression.

Caused by: [moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) -
its zero default reaches moq-transport peers as an immediate expiry of every
object.

Public API: none. Wire: moq-transport SUBSCRIBE_OK, FETCH_OK, and PUBLISH no
longer carry `MAX_CACHE_DURATION` from us; compatible with every published
draft.

## Closes

- [#8465](https://github.com/moq-dev/moq/issues/8465) - a late joiner gets no catalog through a draft-18 relay

## Related

- [moq-transport is a compatibility layer again](/quest/m2/ietf-scope/README.md) - the scope freeze this stays within

# [XS] LOC producers write the duration marker

## Goal

LOC video groups end with the empty frame that closes their last frame's
duration, the same contract the legacy container carries, once every released
LOC consumer skips it.

## Plan

[Duration marker](/quest/m2/duration-marker.md) lands the consumer-side skip
in `rs/moq-mux/src/container/loc` and `js/loc` but leaves LOC producers alone,
because a released LOC consumer submits an empty payload to the decoder. When
the bullet below clears, have the LOC producers write the marker at `cut` and
`finish` exactly as the legacy producer does, and extend the same tests.

## Required

- [Duration marker](/quest/m2/duration-marker.md) - the consumer-side skip and the contract
- A release of `moq-mux` and `@moq/loc` whose consumers skip an empty LOC payload has shipped

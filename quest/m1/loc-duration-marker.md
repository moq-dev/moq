# [XS] LOC producers write the duration marker

## Goal

LOC video groups end with the empty frame that closes their last frame's
duration, the same contract the legacy container carries, once every released
LOC video consumer skips it.

## Plan

The consumer-side skip has landed in `rs/moq-mux/src/container/loc` and
`js/loc`; endpoint recognition is configured for media tracks so empty data
frames remain data. LOC producers are still silent, because a released LOC consumer
submits an empty payload to the decoder. When the bullet below clears, have
the LOC producers write the marker at `cut` and `finish` exactly as the
legacy producer does (`Container::finish_group` on `loc::Wire(Kind::Video)`), and extend the same tests.

## Required

- A release of `moq-mux` and `@moq/loc` whose video consumers skip an empty LOC payload has shipped

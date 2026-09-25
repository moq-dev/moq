# [XS] LOC producers write the duration marker

## Goal

LOC video groups end with the empty frame that closes their last frame's
duration, the same contract the legacy container carries.

## Plan

The consumer-side skip has landed in `rs/moq-mux/src/container/loc` and
`js/loc`, and shipped in `moq-mux` 0.10.3 and `@moq/loc` 0.2.3. Endpoint
recognition is configured for media tracks, so empty data frames remain data.
Have the LOC producers write the marker at `cut` and `finish` exactly as the
legacy producer does (`Container::finish_group` on `loc::Wire(Kind::Video)`),
and extend the same tests.

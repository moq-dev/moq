# [S] Rust SUBSCRIBE honors the Track Properties opt-out

## Goal

A moq-transport SUBSCRIBE that opts out of Track Properties
(INCLUDE_PROPERTIES `0`) gets no TIMESCALE and untimed objects from the Rust
publisher, as from the JS publisher and as Rust FETCH already does
([moq-dev/moq#6999](https://github.com/moq-dev/moq/pull/6999)). The Rust
subscriber asks for properties whenever it needs timestamps, so it reads a
timed track timed from either publisher.

## Plan

Decided 2026-10-27 (maintainer): one rule for both publishers and both
request kinds. Today the Rust subscriber opts out on some SUBSCRIBEs
(`properties_wanted: matches!(target, Target::Request(_))` in
`rs/moq-net/src/ietf/subscriber.rs`) and still expects timestamps, so a JS
publisher would hand it untimed objects on a timed track. Fix the subscriber
first, then the publisher. Land after
[lite-05/06 timestamps](https://github.com/moq-dev/moq/pull/6996), which
changes how moq-transport tracks are stamped.

Test both directions against JS in `just test interop --all`.

Public API: none. Wire: the Rust publisher omits timestamps when asked to.

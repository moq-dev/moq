# [S] JS holds a readvertisement until its withdrawal is handled

## Goal

On drafts 14 to 16, a JS moq-transport session handles a PUBLISH_NAMESPACE
that follows a PUBLISH_NAMESPACE_DONE for the same namespace only after the
withdrawal's handler has finished, as Rust does.

## Plan

[moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530) fixed Rust
(`Readvertised` in `rs/moq-net/src/ietf/adapter.rs`, test
`a_readvertisement_waits_for_the_withdrawal_to_be_handled`); without it
`epochs::ietf14` failed every run. `js/net/src/ietf/adapter.ts` treats
PUBLISH_NAMESPACE_DONE as a plain CloseStream, so a readvertisement can
overtake it. Mirror the hold and its test.

Not planned (2026-10-24, maintainer): the same race per track, where
PUBLISH_DONE routes as CloseStream and a new PUBLISH as NewRequest. It was
found by reading the code and never reproduced.

Public API: none. Wire: none.

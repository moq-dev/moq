# [XS] The fan-out benchmark's session scenario builds again

## Goal

`just bench-fanout` measures its `session` scenario against current `main`
and a base revision, as it does `model`, so a fan-out cost change in the
session layer shows up in its numbers.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8243](https://github.com/moq-dev/moq/pull/8243), whose
measurement could not run the scenario). Milestone: m4, a benchmark nobody
waits on; no Nightly job runs it.

`bench/fanout/session/src/main.rs` imports `moq_net::Hop`, `pub(crate)` since
[moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), and
`origin::Config::new` now takes a `NonZeroU64`. Build the config through the
public constructor (`NonZeroU64::new(hop)`), not a new export. The harness
builds against each revision's own crates, so a base older than #8023 still
needs the old spelling: either pick the constructor by what the revision
exposes in `bench/run.sh`, or state the earliest base it measures.

Public API: none. Wire: none.

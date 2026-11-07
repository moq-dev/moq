# [XS] moq-webrtc drops the ICE wire counters nothing reads

## Goal

moq-webrtc's ICE module no longer counts every packet into `Wire`
(per-packet atomics, public as `moq_ice::Wire` before
[moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)), since only
tests read them.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from #8122): remove the counters
now, and let [Wire stats](/quest/m3/wire-stats/README.md) add an ICE source
with its adapter when it needs one. Not chosen: an ICE child in Wire stats
that wires them into Presence now; leaving them write-only.

Milestone m1: #8122 makes the module crate-private (`mod ice;` in
`rs/moq-webrtc/src/lib.rs`), so removing `Wire` breaks no public API. Start
after #8122 lands.

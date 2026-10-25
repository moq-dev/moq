# moq-congestion

## Goal

One QUIC congestion-control crate, `moq-congestion`, that `moq-quic` and noq
both run, replacing three diverging copies of the same BBRv3 (quinn's
[quinn-rs/quinn#2481](https://github.com/quinn-rs/quinn/pull/2481), noq's
port of it, and `moq-quic`'s port of noq's). moq owns the controllers;
transports own packets, ACKs, and the wire.

Non-goals: quinn integration (quinn#2481 is still in flux, and quinn's
maintainers decide that), and new algorithms; those are the follower quests
listed under Related.

## Plan

Decided with the maintainer on 2026-10-23, after the noq maintainers offered
moq commit access to noq's BBR3 or an external crate:

- **Home.** `rs/moq-congestion` in this monorepo, published by release-plz
  like every other crate, so it shares CI, moq-shaper, and the QUIC sim.
- **Trait redesign.** The controllers now live in the crate with their existing
  callbacks. Redesign that trait separately from the mechanical move.
- **Followers land in the crate.** GCC, SCReAM, the BBR3 delay bound, the
  BBR media study, L4S, and careful resume build on the redesigned trait, and
  the redesign checks its shape against each of them.
- **noq.** After the m1 cut publishes the crate, noq replaces its BBR3 and
  Cubic with it ([noq adopts moq-congestion](/quest/m2/noq-congestion.md)).

## Required

- [Portable controller trait](/quest/m1/congestion/trait.md) - an s2n-style trait with crate-owned per-packet state and pacing, independent of any transport's types

## Related

- [noq adopts moq-congestion](/quest/m2/noq-congestion.md) - the external consumer, after the m1 cut publishes the crate
- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - GCC, SCReAM, and the BBR3 delay bound
- [BBR media study](/quest/m2/quic-bbr-study.md) - BBR3 behavior changes measured against media traffic
- [Skip ProbeRTT after a near-floor RTT](/quest/m1/bbr-probe-rtt-skip.md) - BBR3 behavior change for app-limited media
- [L4S on the backbone](/quest/m3/quic-ecn.md) - an ECT(1) codepoint choice and a scalable CE response
- [Careful resume on reconnect](/quest/m3/quic-careful-resume.md) - seeding a controller from a previous connection

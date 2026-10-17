# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m0 holds urgent fixes and release-line maintenance. m1 is the finite next
breaking release: clustering stability first, the in-tree QUIC stack, and
planned public API and format breaks with their correctness and validation.
m2 holds independently shippable capabilities and optimizations, plus the
publication and adoption steps that follow m1. m3 holds external conditions
and speculative work. A milestone is a priority horizon; m0 maintenance does
not make unrelated infrastructure a gate for the main-to-release cut.

The [release execution](/quest/m2/release/README.md) starts after m1 is ready.
It publishes artifacts before downstream adoption and rollout can complete.
Critical fixes continue to reach the existing release through backports.

## Required

- [m0: immediate priorities](/quest/m0/README.md) - urgent correctness fixes and maintenance of the current release
- [m1: next wave](/quest/m1/README.md) - the agreed next breaking cut, with its required correctness and validation
- [m2: later work](/quest/m2/README.md) - release execution first, then deferred features, optimization, and design studies
- [m3: deferred](/quest/m3/README.md) - gated on the outside world (hardware, a partner, a consumer, a provider's offer, or an upstream release), or speculative with no named consumer

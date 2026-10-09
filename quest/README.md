# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m0 is everything in flight now: relay hardening and the IETF interop
fixes ahead of Seattle, and broadcast epochs. m1 is the next wave across reliability, features, performance, and
planning. m2 holds later features, design studies, and experiments. m3 is
gated on the outside world (hardware, a partner, a consumer, a provider's
offer, or an upstream release) or is speculative work with no named consumer
yet.

A quest waiting on the outside world, in any milestone, requires a small quest
beside it that names the condition. That condition quest stays ready, so
every `/quest-spawn` resurfaces it; when the condition clears, delete it and
move the blocked quest to the milestone its priority belongs in.

## Required

- [m0: immediate priorities](/quest/m0/README.md) - everything in flight now: relay hardening and the IETF interop fixes for Seattle, and broadcast epochs
- [m1: next wave](/quest/m1/README.md) - reliability, capabilities, performance, and the planning that settles their contracts
- [m2: later work](/quest/m2/README.md) - deferred features, design studies, and experiments
- [m3: deferred](/quest/m3/README.md) - gated on the outside world (hardware, a partner, a consumer, a provider's offer, or an upstream release), or speculative with no named consumer

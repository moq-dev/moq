# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m0 is everything in flight now: relay hardening and IETF interop ahead of
Seattle, wildcard routing, and audio playout (jitter target and quality
harness). m1 is the next wave across reliability, features, performance, and
planning. m2 holds later features, design studies, and experiments. m3 is
gated on the outside world: hardware, a partner, a consumer, or a provider's
offer. m4 waits on an upstream release. Priority is
separate from branch targeting: published API and wire breaks still land on dev
under the repository rules.

A quest waiting on the outside world, in any milestone, requires a small quest
beside it that names the condition. That condition quest stays ready, so
every `/quest-spawn` resurfaces it; when the condition clears, delete it and
move the blocked quest to the milestone its priority belongs in.

## Required

- [m0: immediate priorities](/quest/m0/README.md) - everything in flight now: relay hardening and IETF interop for Seattle, wildcard routing, and audio playout
- [m1: next wave](/quest/m1/README.md) - reliability, capabilities, performance, and the planning that settles their contracts
- [m2: later work](/quest/m2/README.md) - deferred features, design studies, and experiments
- [m3: deferred](/quest/m3/README.md) - gated on the outside world: hardware, a partner, a consumer, or a provider's offer
- [m4: upstream](/quest/m4/README.md) - waiting on an upstream release, re-checked periodically

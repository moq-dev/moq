# [S] A stock moq-relay bounds its cache memory

## Goal

moq-relay with no cache settings sizes its group cache to keep 10% of system
memory free (the cgroup limit when set), as `cache.headroom = "10%"` does
today, so an authenticated publisher can't grow the relay without bound by
opening tracks. Setting `capacity` or `headroom` explicitly still works, and
an operator can still choose an unbounded pool explicitly.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the pre-cut flow-control
audit): ✅ default `headroom` to 10% (rejected: a 50% `capacity`, which
ignores the rest of the process's memory; both).

Receive credit bounds only what a connection holds in the transport and in
unfinished frames. A finished frame moves into the shared cache, which is
unbounded in bytes when neither knob is set (`rs/moq-relay/src/cache.rs`).
Each track's live group is exempt from the 30 s idle window and holds up to
its 32 MiB tail, so a publisher holds about 32 MiB per track it opens. Under
pool debt a group sheds its front sooner, so a byte budget bounds it.

Decide how an operator spells "unbounded" without a magic value in the
setting, per AGENTS.md (for example an explicit `capacity = "100%"`, or
`headroom = "0%"`), and say it in the relay docs and the config reference.
Measure process RSS with several publishers opening many tracks against a
small headroom, with the default, so the bound is shown on the built binary.

Behavior change, no `!`. Public API: the relay config default. Wire: none.
`## Caused by`: as old as the cache pool, no single PR.

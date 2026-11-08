# Lifecycles are explicit state

## Goal

Every lifecycle that an audit on 2026-11-07 found encoded implicitly is one
explicit enum or union, in Rust, JS, and the bindings. Each variant carries
its own data, so the combinations that caused bugs can't be represented. The
bugs the audit found in the 11 lifecycles that can reach an invalid
combination, listed in each child, are fixed by these redesigns, and review
and agent rules keep new instances out.

An implicit state machine here is a lifecycle encoded as several flags,
`Option`s, atomics, or locks whose combinations mean phases, where some path
can reach a combination no phase allows. The audit found four recurring
shapes:

1. **A terminal state stored in two or more places**: a flag beside the
   enum, an error beside a live handle, or a second "closed" source.
2. **"Done" marked before the work that can fail**, leaving an object
   half-finished but marked finished.
3. **One-shot handles stored as `Option`s and taken before validation**, so
   "answered" and "failed to answer" look the same.
4. **Per-phase data kept outside an enum that already exists**, guarded by
   `.expect()` or by the order of calls.

A value split across locks or atomics is the same bug as shape 1.

## Plan

Planned 2026-11-07 with the maintainer (/quest-plan).

Decided 2026-11-07 (maintainer):

- ✅ Fix the root cause and redesign freely. In the maintainer's words: "you
  have permission to make any API changes and try to address the root
  cause" and "you have full permission to redesign anything to avoid these
  classes of bug". Not chosen: guarding the one path each bug takes.
- ✅ Fold each bug into the redesign of its shape. Not chosen: small bug
  quests beside a separate redesign epic, or widening each bug quest to its
  own module only. The user-visible fixes therefore land with their
  redesigns.
- ✅ Milestone m1. A child whose design breaks a published surface leaves
  this epic and becomes its own m2 quest, since an epic joins the milestone
  its outcome needs; its PR marks its title `!` and says how to migrate. Not
  chosen: m3 after the cut, or small fixes in m1 with the redesigns in m3.
  This replaces the earlier split from the same session (user-visible fixes
  in m1, internal ones in m3).
- ✅ Guard against new instances in both REVIEW.md and AGENTS.md (see
  [Review and agent rule](/quest/m1/explicit-state/rule.md)). Not chosen:
  REVIEW.md only, or no rule change.
- ✅ `quic::driver::Endpoint::rebind` becomes crate-private, since only
  tests call it (see [Transport close](/quest/m1/explicit-state/transport-close.md)).
  Not chosen: refusing to rebind after close.

Every child reproduces each folded bug in a test before fixing it. A bug
that doesn't reproduce is dropped from the child, and its PR says why. A
child also sweeps the instances listed in it that have no known bug, but
needs no test for those beyond what the redesign changes.

A change to moq-net's per-track or per-group state is on the relay's hot
path: benchmark it (`moq-bench`, comparing quiet cells within one run).

Out of scope: the lite `TrackRun` start shortcut that never sends its
pending drops (`rs/moq-net/src/lite/publisher.rs`, the "start past
everything" branch). It belongs to
[Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md),
which collapses that state and waits on [Drop set](/quest/m1/drop-set.md).

## Required

- [Review and agent rule](/quest/m1/explicit-state/rule.md) - REVIEW.md and AGENTS.md ask for one enum per lifecycle, terminal transitions after fallible work, and one lock per value
- [moq-boy pause state](/quest/m1/explicit-state/boy-pause.md) - a new viewer always wakes a paused emulator
- [Transport close](/quest/m1/explicit-state/transport-close.md) - connection status ends with its error, and every Rust QUIC backend agrees when a connection is closed
- [Video codec drains](/quest/m1/explicit-state/video-drain.md) - a hardware codec whose drain fails is ready for input again or terminally failed, never left stuck
- [Watch playback phases](/quest/m1/explicit-state/watch-playback.md) - watch counts a stall only while a rendition plays
- [Publish capture phases](/quest/m1/explicit-state/publish-capture.md) - dismissing the screen-share picker turns sharing off
- [js/qmux close](/quest/m1/explicit-state/qmux-close.md) - a graceful remote close ends qmux streams cleanly
- [FFI handle state](/quest/m1/explicit-state/ffi-state.md) - FFI handles hold one state each: video follow under one lock, requests answered once, drivers Cancelled after shutdown
- [A failed finish is final](/quest/m1/explicit-state/finish-final.md) - producers and importers are never half-finished, and refuse input after finish
- [Track and group end-of-life](/quest/m1/explicit-state/track-end.md) - moq-net's track and group outcomes are one enum each, in Rust and JS
- [moq-net phase data](/quest/m1/explicit-state/net-phases.md) - moq-net's session and subscription state machines keep each phase's data in its variant
- [moq-mux and moq-gst lifecycles](/quest/m1/explicit-state/mux-gst.md) - export sources, TS and fMP4 export tracks, and gst pads are explicit state
- [A failed catalog finish is final](/quest/m1/explicit-state/catalog-finish.md) - the catalog producer never reports closed with a catalog track left open
- [Video's declared-end freeze is not a stall](/quest/m1/explicit-state/video-declared-end.md) - video holding its last picture through a declared end counts no stall, like audio's declared silence

## Related

- [Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md) - owns the lite `TrackRun` start state and its forgotten-drops bug

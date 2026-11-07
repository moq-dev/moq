# [M] Native playout sync keeps tracks decoupled

## Goal

`moq_mux::timeline` becomes `moq_mux::sync` and follows the browser
player's model: lip sync in the steady state, but one slow track never stalls
or moves another. A wall-clock reference holds the broadcast's delay, each
track converges on it (audio by time-stretching toward it), and audio no
longer owns the anchor. `moq play` (`rs/moq-cli/src/play/`) moves to it, and
the module's duplicate `Kind` gives way to `container::Kind`.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API shape audit):
✅ decouple and rename (rejected: rename only, keeping the audio anchor;
keeping it as is). The 2026-11-02 decision behind
[moq-dev/moq#7759](https://github.com/moq-dev/moq/pull/7759) ("while audio
plays, only audio moves the anchor") is superseded by the 2026-11-05 ruling
on [moq-dev/moq#7941](https://github.com/moq-dev/moq/pull/7941) for the JS
player: "I kind of don't want one slow track to impact everything. We want
lip sync in the steady state but desync during the congestion state."

- ✅ Name: `moq_mux::sync`, matching `@moq/watch`'s `Sync` by role
  (rejected: `presentation`, "presentation sucks"; `playhead`; `schedule`).
  `container::Playout` keeps its name.
- Port the JS semantics from `js/watch/src/sync.ts` (per-track timelines on
  one reference, rework in #7941) rather than inventing a third model.
  `Producer::{audio_restarted, audio_stopped}` and `Consumer::target` likely
  go.
- `moq play`'s timeline tests move with the module and change where audio
  stalls used to hold video; add a case where stalled audio no longer holds
  video.
- moq-dev/studio's moq-scene playout consumes this module through a path
  dependency and adapts on its next pin; this PR names that migration in its
  Impact section.

Breaking: the module is new on main, but release had an unrelated
`moq_mux::timeline`, so the cut's MIGRATE entry says both are gone.

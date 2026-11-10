# [M] A stretch setting bounds how far audio trails before it skips

## Goal

A player chooses how much lateness its audio forgives. `<moq-watch
stretch="2s">` and `moq play --stretch 2s` set how far live audio may trail
the shared clock before it skips ahead. Zero turns the time stretch off: the
player never alters received audio, late audio is skipped, an underrun is
silence, and drift between the publisher's clock and the device's is
corrected by a small skip or pad. The default is 2 s whether `delay` is
`auto` or fixed, so a fixed-delay live player also keeps late audio and
catches up.
`AUTO_MAX_DELAY` is deleted in both `js/watch/src/audio/latency.ts` and
`rs/moq-cli/src/play/args.rs`: every budget it fed (the subscription max
delay, the sink cap, and video retention) is derived from `delay + stretch`.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan). Asked how far audio may trail,
the maintainer answered that it is a product decision and not everybody will
want stretching.

- Shape: ✅ one duration, where zero turns it off (rejected: an on/off switch
  with a fixed bound; no new setting, leaving `auto` at 2 s and a fixed
  delay at none; NetEq's four times the target as the bound). A voice app
  can ask for 200 ms while a sports stream takes 2 s.
- Name: ✅ `stretch` (maintainer; rejected: `catchup`).
- At zero: ✅ no stretching at all (rejected: only falling behind is off,
  with small corrections still stretched). A pitch-period splice is more
  audible on music than on speech, so one value gives unaltered audio.
- Default: ✅ 2 s for `auto` and fixed delays alike (rejected: a fixed delay
  keeping max delay equal to the delay).
- Milestone: ✅ m1 (rejected: m2). The attribute and flag are additive.
- Decided 2026-11-09 (maintainer, /quest-unblock): ✅ delete
  `AUTO_MAX_DELAY` in both players and derive every budget it fed
  (subscription max delay, sink cap, video retention) from `delay + stretch`
  (rejected: keep the 2 s constant beside the setting). The 2 s is an
  arbitrary constant nobody can trace.

The audio subscription's max delay is what makes late audio arrive at all:
the relay and the container consumer drop media older than it. Today it is 2 s
under `auto` (`AUTO_MAX_DELAY` in `js/watch/src/audio/latency.ts` and
`rs/moq-cli/src/play/args.rs`) and the delay itself when fixed, which leaves a
fixed-delay live player nothing to catch up on. The setting feeds that bound:
audio asks for `delay + stretch`.

Recommendations:

- Mirror the name in `js/watch` (`Sync`, the element attribute and property),
  `moq play`, and whatever `moq-audio` type carries the bound, so a Rust
  application gets it without the CLI. `rs/moq-ffi` already takes the decode
  consumer's `delay` min and max and has no playback ring, so no binding
  changes.
- The attribute needs a unit, and an invalid value throws, as `delay` and
  `lookahead` do.
- Say how it composes with `lookahead`: a buffered player already keeps
  everything inside its lookahead, so `stretch` bounds a live player's lag
  and its zero still turns the stretch off in both modes.
- The zero path is the player before the stretch existed. Keep it tested, not
  only reachable.
- Docs: `doc/lib/js/watch.md`, the element's attribute docs, and
  `moq play --help`.

Public API: an additive attribute, property, and flag. Wire: none; a
fixed-delay player's audio subscription asks for a longer max delay.

## Required

- [Stretch model](/quest/m1/audio-jitter-target/stretch.md) - the slow-down and catch-up this setting bounds and turns off

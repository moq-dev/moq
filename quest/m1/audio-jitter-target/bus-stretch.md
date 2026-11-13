# [M] The bus stretches positioned audio onto its clock

## Goal

A `moq_audio::bus` `Positioned` channel follows the epic's rule: given a
stretch bound, it plays onto the positions written to it by the same NetEq
stretch the playback sink uses, so a write that moves later or earlier on the
bus's clock glides there instead of leaving a gap or overlapping. A zero bound
keeps today's exact placement. A moq-side test steps a channel's positions by
a jitter-target step and shows the mix glides onto them with no silence
written and no audio skipped inside the bound.

## Plan

Decided 2026-11-12 (maintainer, moq-dev/studio /quest-plan): ✅ `Positioned`
gains a stretch bound, with zero meaning exact placement (rejected: a separate
channel kind beside `Live` and `Positioned`). It mirrors the playback sink's
`due(at, stretch, jumped)` (`rs/moq-audio/src/playback/sink.rs`), and moq's
API rules forbid a second `write` variant. ✅ m1, in this epic (rejected: m3
with the other bus quests), since it applies the epic's one stretch rule to
the bus.

moq-dev/studio's playout writes each broadcast source's decoded audio to a
`Positioned` channel at the bus position `moq_mux::sync`'s `due` gives it
(moq-dev/studio#137, #144). `Positioned::write` places samples exactly and
fills any distance with silence (`rs/moq-audio/src/bus.rs`), so each 20 ms
step of the jitter target, which moq's traces show jumping as far as 40 to
320 ms, writes that much silence into the studio's program sound, and each
step down skips as much. `moq play` glides through the same steps, since its
sink stretches.

With a nonzero bound, a write is due at its position rather than placed
there: one whose position falls before the channel's queued audio, as after
a step down, or past it, as after a step up, still queues after that audio,
and the channel converges on the position, where today's exact channel
refuses an overlap and pads a gap with silence.

Reuse the ring and stretch the playback sink runs (`ring.rs`, whose
`Consumer::stretch` the sink enables, and `stretch.rs`) rather than a second
implementation; the bus's `Live` channels already queue through that ring,
without the stretch. Audio may trail
its positions by at most the bound, as the sink's may trail its clock, and a
write past the bound plus one window skips onto the clock, as there.

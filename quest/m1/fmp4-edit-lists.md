# [M] fMP4 import applies edit lists to timestamps

## Goal

An fMP4 source with an edit list (AAC priming, a non-zero `media_time`, or an
empty edit) publishes frames at their presentation time, so both players and
every export see the timeline the source meant. The interop AAC rendition
then passes `tone.py --phase` like Opus does.

## Plan

Today `edts` is forwarded in the catalog init but read by no Rust or JS code,
and `rs/moq-mux/src/container/fmp4/export.rs` strips it on the way out. Both
players present `tfdt` plus the composition offset. ffmpeg's fragmented
output does write `elst`: with the interop's `-movflags cmaf`, its AAC track
carries `media_time` 1024 at `tfdt` 0, so every subscriber plays the AAC tone
1024 samples late (`-880us` at 440 Hz once `--phase` checks it), and its Opus
track carries `media_time` equal to the `dOps` pre-skip.

Decided (2026-10-07): the importer applies the edit list to each frame's
timestamp and drops `edts` from the catalog init, so no consumer needs edit
list code. This builds on decoders taking time from the frame timestamp
rather than `tfdt`. Decide in the PR whether priming samples before the edit
are dropped or published with negative-offset handling (Apple's 2112-sample
AAC priming is not a whole number of 1024-sample frames), and refuse edit
lists with more than one non-empty entry rather than guess.

Opus frame timestamps count the OpusHead pre-skip (`draft-lcurley-moq-hang`),
so for Opus the importer applies only the part of the edit beyond the
pre-skip, or the pre-skip is trimmed twice.

Once AAC lines up, drop the `opus` condition from `phase_checked` in
`test/interop/interop.sh` and the matching sentence in
`test/interop/README.md`.

Public API: none. Wire: none, but a behavior change: frame timestamps now
include the edit and the init loses `edts`, so a consumer that offsets AAC
priming on its own must stop, or it shifts twice. Note it in the changelog.

## Related

- [MP4 export](/quest/m2/mp4-export.md) - its open "edit lists" item is the output side

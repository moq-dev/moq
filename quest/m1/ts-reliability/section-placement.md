# [M] Verbatim sections use the PES placement clock

## Goal

Section-framed verbatim frames (SCTE-35 and other private sections) carry
the same source placement time as `pesPacket` frames: the program clock at
the section's first byte on the media timeline. Today they take the latest
video PTS, so they go out a decoder delay later than the source placed
them, and a program without video has no clock for them at import.

## Plan

Reuse the placement the [importer](/quest/m1/ts-reliability/pes-import.md)
derives from the PCR in `SectionStream`, replacing `last_pts`/`start_pts`
and the edge clamp in `SectionStream::emit`. SCTE-35 splice times live in
the section bytes and keep their `pts_adjustment` rebasing.

Export treats the new timestamp the way
[whole-PES export](/quest/m1/ts-reliability/pes-export.md) treats
`pesPacket`: an arrival time on the same placement path, so the first
section byte goes out at the placement and never earlier, with the TS
header and pointer field just before it (not `Track::push`'s decode
deadline and `Schedule`'s early slots), and the
same tune-in comparison against the keyframe's departure, so a section
muxed beside the tune-in keyframe survives.

Export's `build_psi` still refuses sections without a video track; whether
an audio-only program may carry them is
[the fidelity plan](/quest/m1/ts-reliability/fidelity.md)'s call. Update
the existing SCTE-35 import and export tests to the new timestamps, add one
that compares a cue's first section byte position against its source
position and
one with a cue beside the tune-in keyframe, and
reword "the media time it arrived at" in the draft's {{verbatim}} to the
shared placement definition.

## Required

- [Whole-PES import](/quest/m1/ts-reliability/pes-import.md) - the placement clock this reuses

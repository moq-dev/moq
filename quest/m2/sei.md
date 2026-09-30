# [S] Measure whether SEI separation is worthwhile

## Goal

Record enough evidence to decide whether a separate SEI track is worth its
framing and reassembly cost. Keep H.264 and HEVC SEI inline while this is
unresolved; a no-go verdict is a valid outcome.

## Plan

Decided in the 2026-09-30 audit: collapse the line into this study. The
schema, Rust, and web reinsertion quests were deleted; they only make sense
after a positive verdict, and would be re-scoped from it.

Measure SEI payload types, bytes, and cadence on representative H.264 and HEVC
inputs. Separate small timing/display metadata, captions, encoder information,
and arbitrary vendor data. Identify a concrete metadata-only consumer if one
motivates the feature. Do not infer typical savings from a synthetic large
payload or from the codec permitting one.

Compare current inline delivery with the bytes a video-only subscriber could
avoid, including any marker/sidecar overhead. Report total storage separately:
putting the same bytes in two tracks does not itself reduce a complete archive.

Account for recovery points, display metadata, captions, and unknown payloads;
identify what must remain inline or be restored for each supported receiver.
A deadline bounds waiting but cannot prove absent metadata never existed.
Include loss, late arrival, and consumer compatibility in the tradeoff.

Return a recommendation for maintainer agreement. A positive verdict scopes
which payloads move, whether extraction is opt-in, and the association and
latency contract, then files the follow-on quests. A negative verdict ends the
line without affecting captions or unrelated timed-metadata carriage.

## Related

- [Catalog colour model](/quest/m2/color-catalog.md) - preserves display metadata semantics
- [fMP4 emsg](/quest/m2/emsg.md) - independently settles carriage for metadata already outside video
- [CEA-608/708](/quest/m2/captions-cea.md) - can read inline caption SEI without waiting for this experiment

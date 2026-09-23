# [S] Measure whether SEI separation is worthwhile

## Goal

Record enough evidence to decide whether a separate SEI track is worth its
framing and reassembly cost. Keep SEI inline while this is unresolved; a no-go
verdict is a valid outcome.

## Plan

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
Compare inline markers, sidecar coverage, and bounded best-effort joining only
if the use case justifies pursuing a split. A deadline bounds waiting but
cannot prove absent metadata never existed. Include loss, late arrival, and
consumer compatibility in the tradeoff.

Return a recommendation for maintainer agreement. A positive verdict scopes
which payloads to move, whether extraction is opt-in, and the association and
latency contract before the schema or implementation quests start. A negative
verdict abandons the remaining separation quests without affecting unrelated
timed-metadata carriage.

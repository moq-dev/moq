# [S] SEI catalog section

## Goal

Hang defines a top-level `sei` section that relates raw H.264 and H.265 SEI NAL
units to the video access unit they were stripped from. The contract is
sufficient for byte-faithful reinsertion on export and for an application that
subscribes to the sidecar alone.

## Plan

Specify one sidecar track per video rendition, or an equally unambiguous
mapping, and key each sample by the video group sequence and frame ordinal.
That identity is exact by construction and independent of how a splitter
assigns clocks, which a timestamp is not: on the raw Annex B path
`h264::Split::decode` resolves one wall-clock value per call and gives it to
every access unit in that chunk, so several frames can share a timestamp.

Carry the video frame's timestamp on the sample as well, as data rather than
identity. An application syncing to presentation time reads it directly instead
of joining against the video track it deliberately did not subscribe to.

Preserve prefix or suffix placement, original NAL bytes, and order when
several SEI units accompany one access unit. The codec is the mapped video
rendition's, not serialized again in the sidecar. Placement has to be exact, not
approximate: `recovery_point` on the wrong access unit misdirects a receiver's
tune-in, `pic_timing` breaks field cadence and pulldown, and reordered
CEA-608/708 byte pairs garble a stateful caption decoder.

Represent whether an access unit had SEI, so a consumer can tell "there was
none" from "lost, pruned, or not yet arrived". Missing SEI is common and valid,
but an exporter cannot claim a byte-faithful reinsertion it did not make, and
without this signal that loss is unreportable.

Nothing in the video framing changes: the presence signal lives in the sidecar
track's own coverage, not as a flag on video frames, because no consumer blocks
on a sidecar to release a video frame.

Version the schema so a later semantic view can be added without rewriting the
raw contract. Include fixtures for H.264 and H.265 prefix and suffix SEI,
multiple NAL units on one access unit, frames with no SEI, several access units
sharing one timestamp, and group boundaries.

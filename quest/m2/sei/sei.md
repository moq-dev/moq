# [S] SEI catalog section

## Goal

Hang defines a top-level `sei` section that relates raw H.264 and H.265 SEI NAL
units to the video access unit they were stripped from. The contract is
sufficient for byte-faithful reinsertion on export and for an application that
subscribes to the sidecar alone.

## Plan

This section defines the sidecar rule every timed-metadata section follows
(ID3, SCTE-35, emsg, FLV script tags, AV1 metadata OBUs each keep their own
section but share the shape): a metadata track belongs to exactly one
rendition, uses that rendition's group sequence, stamps each frame with the
wire timestamp of the media it accompanies, and carries raw bytes. A 1080p and
a 360p rendition carry different SEI, so there is one sidecar per video
rendition, and group 7 of the sidecar holds the SEI for group 7 of its video.

Metadata that precedes the first media unit (an `emsg` before the first
`moof`, an FLV script tag before the first media tag) rides the rendition's
first group, stamped with its own presentation time when it has one and
otherwise with the first media frame's. A timestamp cannot order it before
that frame, since a tag at timestamp zero and the first frame share one, so
the sidecar frame's placement field, the same one that records prefix versus
suffix SEI, carries an explicit before-first-media value, and an exporter
emits those frames before the first media unit.

Within a group the frame's wire timestamp is the key, so an application
syncing to presentation time reads it directly instead of joining against the
video track it deliberately did not subscribe to. Several access units can
share a timestamp on the raw Annex B path (`h264::Split::decode` resolves one
clock value per call and gives it to every access unit in that chunk), so the
frame's ordinal within the group disambiguates those; it is a tie-break, not
the identity.

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

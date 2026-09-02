# [S] SEI catalog section

## Goal

Hang defines a top-level `sei` section that relates raw H.264 and H.265 SEI NAL
units to the video access unit they were stripped from. The contract is
sufficient for byte-faithful reinsertion on export and for an application that
subscribes to the sidecar alone.

## Plan

Specify one sidecar track per video rendition, or an equally unambiguous
mapping, and key each sample by the timestamp of the video frame it came from.
That timestamp already identifies the access unit within a rendition, so no
second identity is needed, and it is directly usable by an application syncing
data to presentation time.

Preserve codec, prefix or suffix placement, original NAL bytes, and order when
several SEI units accompany one access unit. An access unit with no SEI emits
no sidecar sample; absence is not an error and needs no marker.

Nothing in the video framing changes. The video track carries no presence flag
and no reference to the sidecar, because no consumer blocks on one: the player
does not stitch, and the exporter reinserts whatever it holds for a timestamp.

Version the schema so a later semantic view can be added without rewriting the
raw contract. Include fixtures for H.264 and H.265 prefix and suffix SEI,
multiple NAL units on one access unit, frames with no SEI, and group
boundaries.

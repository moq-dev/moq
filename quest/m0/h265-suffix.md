# [S] H.265 suffix SEI ownership

## Goal

H.265 suffix SEI NAL units remain on the access unit they follow, including the
last access unit at EOF. They never move onto the next picture or disappear
because a splitter closed the current unit too early.

## Plan

Fix access-unit assembly in `moq-mux` so prefix SEI participates in opening the
following picture while suffix SEI appends to the current picture. Preserve
multiple prefix and suffix NAL units and their order around VCL data.

Add focused Annex B and length-prefixed tests covering consecutive pictures,
multiple suffix units, suffix at EOF, and prefix immediately after suffix. Run
the same fixtures through MPEG-TS import and export so the unit test cannot pass
while the container seam still loses ownership.

# [S] moq-transcode finishes the group it is transcoding

## Goal

When demand for a transcoded rendition ends mid-group, `moq-transcode`
finishes the open output group before leaving its source session. A later
fetch of that sequence gets the complete group, never an aborted one.

## Plan

Since #5820 a fetch counts as demand for its whole lifetime, so a fetch
starts the rung's live loop. When the fetch ends, the loop sees the track
unused and aborts its open output group with `Error::Cancel`; a later fetch
of that sequence hits the aborted group. #5820's tests worked around it by
holding queries open.

Decided 2026-10-17 (maintainer): finish the group, then leave. A group name
always means the same content, so the published group must be the whole
group for every later reader, not whatever was transcoded before demand
went away. Rejected: aborting and re-transcoding on the next fetch (two
contents under one name), and serving fetches on a separate path (more
machinery for the same contract).

Drop the held-query workaround in the tests once the loop finishes groups;
a regression test fetches a sequence after an earlier fetch that started
the live loop has ended.

Public API: none. Wire: none.
